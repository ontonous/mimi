//! Typed production-island eligibility predicates.
//!
//! These predicates are shared by route selection and legacy deletion gates.
//! They intentionally inspect only checker-owned facts; a backend must not
//! rediscover a migrated shape from the retained surface AST.

use crate::ast::Type;
use crate::core::ir::{
    ResolvedBinaryOp, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedLiteral,
    ResolvedPatternKind, ResolvedProjection, ResolvedStmtKind, ResolvedValueProjection,
};
use crate::core::{CheckedProgram, NodeId, ResolvedBody, TransitionId};

/// Whether `program` contains an S8-shaped Flow transition candidate.
///
/// S8 is deliberately narrower than "a Flow that happens to compile": one
/// import-free Flow, one user transition, one non-Fault state, a concrete
/// `i32` payload, and a silent self-loop with no failure, parameter, pinned,
/// persistent, or actor boundary. The default dispatcher uses this typed
/// candidate predicate so a candidate whose body has not passed every
/// consumer preflight is rejected rather than silently returning to legacy.
pub fn is_s8_flow_transition_candidate(program: &CheckedProgram) -> bool {
    if program.has_imports() || program.flows().len() != 1 || !program.actors().is_empty() {
        return false;
    }

    let implemented = program
        .transitions()
        .values()
        .filter(|transition| {
            !transition.is_fallback && program.resolved_body(&transition.node_id).is_some()
        })
        .collect::<Vec<_>>();
    let [transition] = implemented.as_slice() else {
        return false;
    };

    let Some(flow) = program.flows().get(&transition.id.flow) else {
        return false;
    };
    let Some(source_state) = flow.states.get(&transition.id.source.name) else {
        return false;
    };
    let [(_, source_ty)] = source_state.payload.as_slice() else {
        return false;
    };
    let Some(target) = transition.targets.first() else {
        return false;
    };
    let Some(target_state) = flow.states.get(&target.name) else {
        return false;
    };

    transition.silent_transition
        && transition.targets.len() == 1
        && transition.targets[0] == transition.id.source
        && transition.params.is_empty()
        && transition.fails.is_none()
        && !transition.is_fallback
        && !transition.is_ffi_pinned
        && flow
            .states
            .keys()
            .filter(|name| name.as_str() != "Fault")
            .count()
            == 1
        && flow
            .transitions
            .iter()
            .filter(|id| {
                program
                    .transitions()
                    .get(*id)
                    .is_some_and(|item| !item.is_fallback)
            })
            .count()
            == 1
        && flow.persistent_fields.is_empty()
        && target_state.payload.len() == 1
        && is_concrete_i32_type(source_ty)
        && target_state
            .payload
            .first()
            .is_some_and(|(_, ty)| is_concrete_i32_type(ty))
}

/// Whether `program` is the exact S8 production island already closed by all
/// four consumers. This is stricter than the candidate predicate: the
/// admitted bodies contain only typed scalar/record expressions and the one
/// transition call. A candidate with `println`, a container, a variant,
/// control flow, an effect, or another call remains a candidate for the
/// dispatcher's fail-closed preflight, but does not enter the deleted native
/// compatibility route.
pub fn is_exact_s8_flow_transition(program: &CheckedProgram) -> bool {
    if !is_s8_flow_transition_candidate(program) {
        return false;
    }

    let implemented = program
        .transitions()
        .values()
        .filter(|transition| {
            !transition.is_fallback && program.resolved_body(&transition.node_id).is_some()
        })
        .collect::<Vec<_>>();
    let [transition] = implemented.as_slice() else {
        return false;
    };
    let Some(main) = program.resolved_body(&NodeId("function:main".into())) else {
        return false;
    };
    let Some(transition_body) = program.resolved_body(&transition.node_id) else {
        return false;
    };
    let mut saw_transition_call = false;
    let only_closed_island_callables = program.functions().values().all(|function| {
        if function.node_id == NodeId("function:main".into())
            || function.node_id == transition.node_id
        {
            return true;
        }
        // The CLI installs the checker-known prelude before checking. Prelude
        // helpers are declaration dependencies outside the user S8 island;
        // every other callable must be accounted for by the whole-program
        // admission rather than silently omitted from the verifier receipt.
        program
            .source_registry()
            .key(function.origin.user_span().source_id)
            .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
    });
    only_closed_island_callables
        && is_exact_s8_body(main, &transition.id, &mut saw_transition_call)
        && is_exact_s8_body(transition_body, &transition.id, &mut saw_transition_call)
        && saw_transition_call
}

/// The S8 native island is the closed, dependency-free transition example,
/// not every program that declares a compatible Flow.  This body predicate is
/// deliberately expressed over ResolvedBody facts: adding a builtin, a
/// container/variant operation, a control-flow expression, or an ordinary call
/// keeps the caller on the explicit compatibility route until that shape has
/// its own all-consumer contract.
fn is_exact_s8_body(
    body: &ResolvedBody,
    transition: &TransitionId,
    saw_transition_call: &mut bool,
) -> bool {
    body.captures.is_empty()
        && body.default_values.is_empty()
        && is_exact_s8_block(&body.root, transition, saw_transition_call)
}

fn is_exact_s8_block(
    block: &crate::core::ir::ResolvedBlock,
    transition: &TransitionId,
    saw_transition_call: &mut bool,
) -> bool {
    block
        .statements
        .iter()
        .all(|statement| match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                matches!(
                    &pattern.kind,
                    ResolvedPatternKind::Binding {
                        by_reference: None,
                        ..
                    }
                ) && initializer.as_ref().is_some_and(|initializer| {
                    is_exact_s8_expr(initializer, transition, saw_transition_call)
                })
            }
            ResolvedStmtKind::Return { value, .. } => value
                .as_ref()
                .is_some_and(|value| is_exact_s8_expr(value, transition, saw_transition_call)),
            _ => false,
        })
        && block
            .result
            .as_deref()
            .is_none_or(|result| is_exact_s8_expr(result, transition, saw_transition_call))
}

fn is_exact_s8_expr(
    expression: &ResolvedExpr,
    transition: &TransitionId,
    saw_transition_call: &mut bool,
) -> bool {
    if !expression.effects.is_empty() || !expression.backend_requirements.is_empty() {
        return false;
    }
    match &expression.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Int(_)) => true,
        ResolvedExprKind::Load(crate::core::ir::ResolvedPlace { projections, .. }) => projections
            .iter()
            .all(|projection| matches!(projection, ResolvedProjection::Field { .. })),
        ResolvedExprKind::Project { value, projection } => {
            matches!(projection, ResolvedValueProjection::Field(_))
                && is_exact_s8_expr(value, transition, saw_transition_call)
        }
        ResolvedExprKind::Binary {
            op: ResolvedBinaryOp::Add,
            left,
            right,
        } => {
            is_exact_s8_expr(left, transition, saw_transition_call)
                && is_exact_s8_expr(right, transition, saw_transition_call)
        }
        ResolvedExprKind::Call(call) => {
            let ResolvedCallee::Transition(callee) = &call.callee else {
                return false;
            };
            if callee != transition {
                return false;
            }
            *saw_transition_call = true;
            call.arguments
                .iter()
                .all(|argument| is_exact_s8_expr(&argument.value, transition, saw_transition_call))
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            rest.is_none()
                && fields
                    .iter()
                    .all(|field| is_exact_s8_expr(&field.value, transition, saw_transition_call))
        }
        _ => false,
    }
}

fn is_concrete_i32_type(ty: &Type) -> bool {
    match ty {
        Type::Located { ty, .. } => is_concrete_i32_type(ty),
        Type::Name(name, arguments) => name == "i32" && arguments.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_exact_s8_flow_transition, is_s8_flow_transition_candidate};

    fn checked(source: &str) -> crate::core::CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file).expect("check")
    }

    #[test]
    fn recognizes_only_the_typed_s8_shape() {
        let program = checked(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }",
        );
        assert!(is_exact_s8_flow_transition(&program));
    }

    #[test]
    fn rejects_an_imported_or_non_i32_flow() {
        let program = checked(
            "flow Counter { state Zero { n: i64 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i64 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }",
        );
        assert!(!is_exact_s8_flow_transition(&program));
    }

    #[test]
    fn keeps_unsupported_body_as_candidate_but_outside_exact_island() {
        let program = checked(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) println(c2.n) c2.n }",
        );
        assert!(is_s8_flow_transition_candidate(&program));
        assert!(!is_exact_s8_flow_transition(&program));
    }

    #[test]
    fn rejects_an_unrelated_user_callable_from_the_exact_island() {
        let program = checked(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func helper() -> i32 { println(7) 7 } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }",
        );
        assert!(is_s8_flow_transition_candidate(&program));
        assert!(!is_exact_s8_flow_transition(&program));
    }
}
