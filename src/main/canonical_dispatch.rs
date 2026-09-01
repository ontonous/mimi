//! Program-level Canonical MIR route selection.
//!
//! The default entry points must make one capability decision for the whole
//! checked program.  A canonical backend is never attempted and then replaced
//! by legacy code on failure: programs either pass this preflight and use the
//! canonical route, or remain on the legacy route because their capability
//! set is not yet migrated.  Once the scalar collection island is recognized,
//! its failure is an explicit rejection rather than compatibility fallback.

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
    /// A recognized migrated-island candidate failed canonical preflight.
    /// Once an island is recognized, its old production path is deleted and
    /// the CLI must fail closed instead of silently compiling it with legacy.
    Rejected(String),
}

/// Build the production MIR graph while excluding only the known prelude
/// compatibility source.  User and imported sources remain in the graph and
/// are all required to lower and validate.
pub(crate) fn build_canonical_program(
    checked: &CheckedProgram,
    merged_file: &File,
) -> Result<MirProgram, String> {
    build_canonical_program_for_sources(checked, merged_file, None)
}

/// Build the same canonical graph as the production dispatcher, optionally
/// restricting the graph to a source scope for `mimi mir` inspection.  The
/// source filter is a graph-selection concern only; lowering, generic
/// instance materialization, TypeDesc construction, and validation all remain
/// in the single `MirProgram` production constructor.
pub(crate) fn build_canonical_program_for_sources(
    checked: &CheckedProgram,
    merged_file: &File,
    included_sources: Option<&HashSet<mimi::span::SourceId>>,
) -> Result<MirProgram, String> {
    let excluded_sources = merged_file
        .sources
        .records()
        .iter()
        .filter(|record| {
            record.key.as_str() == "stdlib:prelude.mimi"
                || included_sources.is_some_and(|included| !included.contains(&record.id))
        })
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    MirProgram::from_checked_program_excluding_sources(checked, &excluded_sources)
        .map_err(|error| format!("canonical MIR build error: {error:?}"))
}

/// Select a default route for a complete checked program.
///
/// The current default-switch islands are deliberately narrow: a program must
/// contain either a checker-selected scalar Set facade instance, a flat Copy
/// record value, or a concrete scalar `List.len` operation. The candidate then
/// has to pass every consumer preflight before any caller starts execution or
/// LLVM emission. Returning `Legacy` means the program has not entered a
/// migrated island. Returning `Rejected` means an island was recognized but
/// canonical preflight failed; callers must report the error and must not
/// invoke legacy.
pub(crate) fn select_default_route(
    checked: &CheckedProgram,
    merged_file: &File,
) -> DefaultMirRoute {
    let set_candidate = may_contain_typed_set_facade(checked, merged_file);
    let record_candidate = may_contain_user_record(checked, merged_file);
    let flow_candidate = may_contain_single_silent_local_transition(checked, merged_file);
    // List.len is probed only for import-free programs. The canonical graph
    // itself is the typed fact source: after lowering, an actual ListOp::Len
    // is evidence that the shared TypeDesc contract admitted the operation.
    let list_len_probe_allowed = merged_file.imports.is_empty();
    if !set_candidate && !record_candidate && !list_len_probe_allowed && !flow_candidate {
        return DefaultMirRoute::Legacy;
    }

    let canonical = match build_canonical_program(checked, merged_file) {
        Ok(canonical) => canonical,
        Err(error) => {
            return reject_migrated_candidates(
                flow_candidate,
                set_candidate,
                false,
                format!("canonical MIR construction failed: {error}"),
            )
        }
    };
    let set_instance = canonical.instances().values().any(|instance| {
        matches!(
            instance.contract,
            MirGenericInstanceContract::ScalarSetFacade { .. }
        )
    });
    let copy_record = mimi::core::mir::contains_flat_copy_record_candidate(&canonical);
    let list_len_operation = canonical_has_list_len(&canonical);
    let collection_candidate = mimi::core::mir::contains_scalar_collection_candidate(&canonical);
    let flow_transition_operation = canonical_has_flow_transition(&canonical);
    if (!set_candidate || !set_instance)
        && (!record_candidate || !copy_record)
        && (!list_len_probe_allowed || !list_len_operation)
        && (!flow_candidate || !flow_transition_operation)
    {
        return reject_migrated_candidates(
            flow_candidate,
            collection_candidate || (record_candidate && copy_record),
            record_candidate && copy_record,
            "canonical graph did not materialize the selected production operation",
        );
    }

    // S11: the production unit is a complete scalar List/Set executable
    // graph, not an individual opcode.  The island validator consumes only
    // canonical MIR and TypeDesc facts and runs before any verifier/backend
    // preflight.  A real materialized Set facade or List.len operation is
    // therefore either inside this finite envelope or rejected; it cannot
    // re-enter the legacy route.
    if collection_candidate {
        if let Err(errors) = mimi::core::mir::validate_scalar_collection_island(&canonical) {
            return reject_migrated_candidates(
                flow_candidate,
                true,
                false,
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::SCALAR_COLLECTION_ISLAND
                ),
            );
        }
    }

    // The MIR verifier intentionally skips bodies with no contract.  That is
    // not permission for an unsupported instruction to enter a default
    // native/bytecode island.  Scan the complete canonical graph before the
    // verifier's contract pass so every selected consumer has an explicit
    // capability, including no-obligation functions.
    if let Err(error) = mimi::verifier::validate_mir_capabilities(&canonical) {
        return reject_migrated_candidates(
            flow_candidate,
            collection_candidate,
            record_candidate && copy_record,
            format!("verifier capability gate failed: {error:?}"),
        );
    }

    // Bytecode and native are both checked only after the verifier capability
    // gate.  The actual consumers repeat their own validation immediately
    // before use.
    if let Err(errors) = mimi::interp::bytecode::compile_mir_program(&canonical) {
        return reject_migrated_candidates(
            flow_candidate,
            collection_candidate,
            record_candidate && copy_record,
            format!("MIR-bytecode preflight failed: {errors:?}"),
        );
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&canonical) {
        return reject_migrated_candidates(
            flow_candidate,
            collection_candidate,
            record_candidate && copy_record,
            format!("native MIR preflight failed: {errors:?}"),
        );
    }

    // The verifier is a fourth consumer of the same program.  A definitive
    // disproven result is still a valid verifier observation; an unsupported
    // or inconclusive verifier result means this program is not yet a complete
    // default-switch island.
    let verifier_ready = match mimi::verifier::verify_mir(&canonical, String::new()) {
        Ok(results) => results.iter().all(|result| {
            matches!(
                result.status,
                VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
            )
        }),
        Err(error) => {
            return reject_migrated_candidates(
                flow_candidate,
                collection_candidate,
                record_candidate && copy_record,
                format!("verifier contract pass failed: {error}"),
            )
        }
    };
    if !verifier_ready {
        return reject_migrated_candidates(
            flow_candidate,
            collection_candidate,
            record_candidate && copy_record,
            "verifier returned an unsupported or inconclusive result",
        );
    }

    DefaultMirRoute::Canonical(canonical)
}

fn reject_migrated_candidates(
    flow_candidate: bool,
    collection_candidate: bool,
    record_candidate: bool,
    reason: impl Into<String>,
) -> DefaultMirRoute {
    if flow_candidate {
        DefaultMirRoute::Rejected(format!(
            "S8 Flow transition candidate is not eligible for the default route: {}",
            reason.into()
        ))
    } else if collection_candidate {
        DefaultMirRoute::Rejected(format!(
            "S11 scalar collection candidate is not eligible for the default route: {}",
            reason.into()
        ))
    } else if record_candidate {
        DefaultMirRoute::Rejected(format!(
            "S0 flat Copy record candidate is not eligible for the default route: {}",
            reason.into()
        ))
    } else {
        DefaultMirRoute::Legacy
    }
}

fn canonical_has_list_len(canonical: &MirProgram) -> bool {
    canonical.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    mimi::core::mir::MirInstructionKind::ListOp {
                        operation: mimi::core::mir::MirListOperation::Len,
                        ..
                    }
                )
            })
        })
    })
}

fn canonical_has_flow_transition(canonical: &MirProgram) -> bool {
    canonical.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    mimi::core::mir::MirInstructionKind::FlowTransition { .. }
                )
            })
        })
    })
}

fn may_contain_single_silent_local_transition(
    checked: &CheckedProgram,
    merged_file: &File,
) -> bool {
    // The typed candidate predicate is shared with the native exact-island
    // gate. Keep the merged-file check as a front-end provenance guard: a caller may
    // construct a CheckedProgram before module merging has been reflected in
    // its directory, but an imported program is never the exact S8 island.
    if !merged_file.imports.is_empty() {
        return false;
    }
    mimi::core::mir::is_s8_flow_transition_candidate(checked)
}

fn may_contain_user_record(checked: &CheckedProgram, merged_file: &File) -> bool {
    let excluded_sources = merged_file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    checked.type_defs().values().any(|type_def| {
        type_def.kind == mimi::core::ResolvedTypeKind::Record
            && !excluded_sources.contains(&type_def.origin.user_span().source_id)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> (CheckedProgram, File) {
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = mimi::core::check_program(&file).expect("check");
        (checked, file)
    }

    #[test]
    fn copy_record_island_passes_all_default_consumer_gates() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_record_copy.mimi"
        ));
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn single_silent_local_flow_transition_switches_as_one_complete_island() {
        let source = "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("the closed silent-local Flow island should select canonical MIR");
        };
        assert_eq!(program.transitions().len(), 1);
        assert!(canonical_has_flow_transition(&program));
    }

    #[test]
    fn rejected_flow_candidate_cannot_reenter_legacy_route() {
        let source = "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) println(c2.n) c2.n }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("a recognized Flow candidate must fail closed instead of using legacy");
        };
        assert!(reason.contains("S8 Flow transition candidate"));
    }

    #[test]
    fn rejected_mixed_record_and_owned_variant_cannot_reenter_legacy_route() {
        let source = r#"
            type Point { x: i32 }

            func make_some() -> Option<string> { Some("owned") }

            func main() -> i32 {
                let point = Point { x: 1 }
                point.x
            }
        "#;
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!(
                "a recognized flat Copy-record candidate must fail closed instead of using legacy"
            );
        };
        assert!(reason.contains("S0 flat Copy record candidate"));
    }

    #[test]
    fn scalar_collection_candidate_rejects_a_mixed_managed_graph() {
        let source = r#"
            func main() -> i32 {
                let values = [1, 2, 3]
                let count = len(values)
                drop(values)
                let text = "outside"
                drop(text)
                count
            }
        "#;
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("a List.len candidate with a managed value must fail closed");
        };
        assert!(reason.contains("S11 scalar collection candidate"));
        assert!(reason.contains("copy-scalar-collection-v1"));
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
