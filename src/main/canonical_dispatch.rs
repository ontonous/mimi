//! Program-level Canonical MIR route selection.
//!
//! The default entry points must make one capability decision for the whole
//! checked program.  A canonical backend is never attempted and then replaced
//! by legacy code on failure: programs either pass this preflight and use the
//! canonical route, or remain on the legacy route because their capability
//! set is not yet migrated.

use std::collections::HashSet;

use mimi::ast::File;
use mimi::core::ir::{ResolvedType, ResolvedTypeId};
use mimi::core::mir::reference::MirProgram;
use mimi::core::mir::MirGenericInstanceContract;
use mimi::core::CheckedProgram;
use mimi::verifier::VerifStatus;

pub(crate) enum DefaultMirRoute {
    Legacy,
    Canonical(MirProgram),
}

/// Build the production MIR graph while excluding only the known prelude
/// compatibility source.  User and imported sources remain in the graph and
/// are all required to lower and validate.
pub(crate) fn build_canonical_program(
    checked: &CheckedProgram,
    merged_file: &File,
) -> Result<MirProgram, String> {
    let excluded_sources = merged_file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    MirProgram::from_checked_program_excluding_sources(checked, &excluded_sources)
        .map_err(|error| format!("canonical MIR build error: {error:?}"))
}

/// Select a default route for a complete checked program.
///
/// The current default-switch island is deliberately narrow: it must contain
/// a checker-selected scalar Set facade instance.  The candidate then has to
/// pass every consumer preflight before any caller starts execution or LLVM
/// emission.  Returning `Legacy` means the program has not entered this
/// migrated island; it is not a backend fallback after a canonical emission
/// attempt.
pub(crate) fn select_default_route(
    checked: &CheckedProgram,
    merged_file: &File,
) -> DefaultMirRoute {
    if !may_contain_typed_set_facade(checked, merged_file) {
        return DefaultMirRoute::Legacy;
    }

    let Ok(canonical) = build_canonical_program(checked, merged_file) else {
        return DefaultMirRoute::Legacy;
    };
    if !canonical.instances().values().any(|instance| {
        matches!(
            instance.contract,
            MirGenericInstanceContract::ScalarSetFacade { .. }
        )
    }) {
        return DefaultMirRoute::Legacy;
    }

    // Bytecode and native are both checked before the route is selected.  The
    // actual consumers repeat their own validation immediately before use.
    if mimi::interp::bytecode::compile_mir_program(&canonical).is_err()
        || mimi::codegen::mir::validate_mir_native(&canonical).is_err()
    {
        return DefaultMirRoute::Legacy;
    }

    // The verifier is a fourth consumer of the same program.  A definitive
    // disproven result is still a valid verifier observation; an unsupported
    // or inconclusive verifier result means this program is not yet a complete
    // default-switch island.
    let verifier_ready = mimi::verifier::verify_mir(&canonical, String::new())
        .map(|results| {
            results.iter().all(|result| {
                matches!(
                    result.status,
                    VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
                )
            })
        })
        .unwrap_or(false);
    if !verifier_ready {
        return DefaultMirRoute::Legacy;
    }

    DefaultMirRoute::Canonical(canonical)
}

fn may_contain_typed_set_facade(checked: &CheckedProgram, merged_file: &File) -> bool {
    let excluded_sources = merged_file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    let types = checked.resolved_types();

    checked.callables().values().any(|callable| {
        if excluded_sources.contains(&callable.body.root.origin.user_span().source_id)
            || callable.signature.generic_parameters.is_empty()
        {
            return false;
        }
        callable.signature.parameters.iter().any(|parameter| {
            mentions_generic_set(
                &parameter.ty,
                types,
                &callable.signature.generic_parameters,
                &mut HashSet::new(),
            )
        }) || mentions_generic_set(
            &callable.signature.result,
            types,
            &callable.signature.generic_parameters,
            &mut HashSet::new(),
        )
    })
}

fn mentions_generic_set(
    id: &ResolvedTypeId,
    types: &mimi::core::ir::ResolvedTypeTable,
    generic_parameters: &[mimi::core::NodeId],
    seen: &mut HashSet<ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return false;
    }
    let Some(ty) = types.get(id) else {
        return false;
    };
    match ty {
        ResolvedType::Nominal {
            item, arguments, ..
        } => {
            (item.as_str() == "builtin:type:Set"
                && arguments.iter().any(|argument| {
                    contains_generic_parameter(
                        argument,
                        types,
                        generic_parameters,
                        &mut HashSet::new(),
                    )
                }))
                || arguments
                    .iter()
                    .any(|argument| mentions_generic_set(argument, types, generic_parameters, seen))
        }
        ResolvedType::Option(inner)
        | ResolvedType::CBuffer(inner)
        | ResolvedType::Ownership { target: inner, .. }
        | ResolvedType::Newtype { inner, .. }
        | ResolvedType::Slice(inner)
        | ResolvedType::RawPointer { target: inner, .. } => {
            mentions_generic_set(inner, types, generic_parameters, seen)
        }
        ResolvedType::Result { ok, error } => {
            mentions_generic_set(ok, types, generic_parameters, seen)
                || mentions_generic_set(error, types, generic_parameters, seen)
        }
        ResolvedType::Tuple(items) => items
            .iter()
            .any(|item| mentions_generic_set(item, types, generic_parameters, seen)),
        ResolvedType::Array { element, .. } => {
            mentions_generic_set(element, types, generic_parameters, seen)
        }
        ResolvedType::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(|parameter| mentions_generic_set(parameter, types, generic_parameters, seen))
                || mentions_generic_set(result, types, generic_parameters, seen)
        }
        _ => false,
    }
}

fn contains_generic_parameter(
    id: &ResolvedTypeId,
    types: &mimi::core::ir::ResolvedTypeTable,
    generic_parameters: &[mimi::core::NodeId],
    seen: &mut HashSet<ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return false;
    }
    match types.get(id) {
        Some(ResolvedType::GenericParameter(parameter)) => generic_parameters.contains(parameter),
        Some(ResolvedType::Nominal { arguments, .. }) => arguments
            .iter()
            .any(|argument| contains_generic_parameter(argument, types, generic_parameters, seen)),
        Some(ResolvedType::Option(inner))
        | Some(ResolvedType::CBuffer(inner))
        | Some(ResolvedType::Ownership { target: inner, .. })
        | Some(ResolvedType::Newtype { inner, .. })
        | Some(ResolvedType::Slice(inner))
        | Some(ResolvedType::RawPointer { target: inner, .. }) => {
            contains_generic_parameter(inner, types, generic_parameters, seen)
        }
        Some(ResolvedType::Result { ok, error }) => {
            contains_generic_parameter(ok, types, generic_parameters, seen)
                || contains_generic_parameter(error, types, generic_parameters, seen)
        }
        Some(ResolvedType::Tuple(items)) => items
            .iter()
            .any(|item| contains_generic_parameter(item, types, generic_parameters, seen)),
        Some(ResolvedType::Function {
            parameters, result, ..
        }) => {
            parameters.iter().any(|parameter| {
                contains_generic_parameter(parameter, types, generic_parameters, seen)
            }) || contains_generic_parameter(result, types, generic_parameters, seen)
        }
        Some(ResolvedType::Array { element, .. }) => {
            contains_generic_parameter(element, types, generic_parameters, seen)
        }
        _ => false,
    }
}
