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
use mimi::core::mir::reference::MirProgram;
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
    // Admission is checker-owned and must be decided before MIR construction.
    // Keep the dispatcher from growing a second Set/record type walk: the
    // same admission result is also used by the public verifier boundary.
    let collection_admission = mimi::core::mir::classify_scalar_collection_admission(checked);
    // Imported stdlib facades are part of the production island once their
    // concrete operations materialize in MIR.  Do not use the retained File
    // import list as a second route policy: checker admission records the
    // mixed provenance, while the canonical MIR island validator decides
    // whether the complete imported executable graph is covered.  This is
    // what lets `std::set` enter the same default Set island without opening
    // an implicit legacy fallback for an unsupported imported graph.
    let collection_hint = !matches!(
        collection_admission,
        mimi::core::mir::ScalarCollectionAdmission::OutsideProfile
    );
    let complete_collection_candidate = matches!(
        collection_admission,
        mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
    );
    let record_admission = mimi::core::mir::classify_flat_copy_record_admission(checked);
    let record_hint = !matches!(
        record_admission,
        mimi::core::mir::FlatCopyRecordAdmission::OutsideProfile
    );
    let complete_record_candidate = matches!(
        record_admission,
        mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
    );
    let flow_candidate = may_contain_single_silent_local_transition(checked, merged_file);
    if !collection_hint && !record_hint && !flow_candidate {
        return DefaultMirRoute::Legacy;
    }

    let canonical = match build_canonical_program(checked, merged_file) {
        Ok(canonical) => canonical,
        Err(error) => {
            return reject_migrated_candidates(
                flow_candidate,
                complete_collection_candidate,
                complete_record_candidate,
                format!("canonical MIR construction failed: {error}"),
            )
        }
    };
    let copy_record = mimi::core::mir::contains_flat_copy_record_candidate(&canonical);
    let materialized_collection_candidate =
        mimi::core::mir::contains_scalar_collection_candidate(&canonical);
    let flow_transition_operation =
        mimi::core::mir::contains_s8_flow_transition_candidate(&canonical);
    // Mixed coverage remains a compatibility boundary only when construction
    // proves that no migrated operation was materialized.  A Complete
    // admission missing its receipt, however, is a hard route failure.
    let collection_route_candidate =
        complete_collection_candidate || (collection_hint && materialized_collection_candidate);
    let record_route_candidate = complete_record_candidate || (record_hint && copy_record);
    if (complete_collection_candidate && !materialized_collection_candidate)
        || (complete_record_candidate && !copy_record)
        || (flow_candidate && !flow_transition_operation)
    {
        return reject_migrated_candidates(
            flow_candidate,
            collection_route_candidate || (record_route_candidate && copy_record),
            record_route_candidate,
            "canonical graph did not materialize the selected production operation",
        );
    }

    // A mixed program is not a partial canonical program.  If its graph does
    // contain a migrated boundary, keep the old path deleted for that
    // boundary and reject the whole route.  If it contains no such operation,
    // it remains an explicit compatibility input and may use Legacy.
    if record_route_candidate && !complete_record_candidate {
        return reject_migrated_candidates(
            flow_candidate,
            collection_route_candidate,
            true,
            "flat Copy record materialized inside mixed coverage",
        );
    }
    if !collection_route_candidate && !record_route_candidate && !flow_candidate {
        return DefaultMirRoute::Legacy;
    }

    // S11: the production unit is a complete scalar List/Set executable
    // graph, not an individual opcode.  The island validator consumes only
    // canonical MIR and TypeDesc facts and runs before any verifier/backend
    // preflight.  A real materialized Set facade or List.len operation is
    // therefore either inside this finite envelope or rejected; it cannot
    // re-enter the legacy route.
    if materialized_collection_candidate {
        if let Err(errors) = mimi::core::mir::validate_scalar_collection_island(&canonical) {
            return reject_migrated_candidates(
                flow_candidate,
                true,
                record_route_candidate,
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
            collection_route_candidate,
            record_route_candidate,
            format!("verifier capability gate failed: {error:?}"),
        );
    }

    // Bytecode and native are both checked only after the verifier capability
    // gate.  The actual consumers repeat their own validation immediately
    // before use.
    if let Err(errors) = mimi::interp::bytecode::compile_mir_program(&canonical) {
        return reject_migrated_candidates(
            flow_candidate,
            collection_route_candidate,
            record_route_candidate,
            format!("MIR-bytecode preflight failed: {errors:?}"),
        );
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&canonical) {
        return reject_migrated_candidates(
            flow_candidate,
            collection_route_candidate,
            record_route_candidate,
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
                collection_route_candidate,
                record_route_candidate,
                format!("verifier contract pass failed: {error}"),
            )
        }
    };
    if !verifier_ready {
        return reject_migrated_candidates(
            flow_candidate,
            collection_route_candidate,
            record_route_candidate,
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
    fn copy_record_update_is_complete_and_uses_canonical_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_record_update.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
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
        assert!(mimi::core::mir::contains_s8_flow_transition_candidate(
            &program
        ));
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
    fn complete_record_materialization_failure_cannot_reenter_legacy_route() {
        let source = r#"
            type Point { x: i32 }

            func make_fn(p: Point) -> func(i32) -> i32 {
                fn(value: i32) -> i32 { value + 1 }
            }

            func main() -> i32 { 0 }
        "#;
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("complete record admission must reject a failed MIR construction");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
        assert!(
            reason.contains("canonical MIR construction failed"),
            "{reason}"
        );
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

    #[test]
    fn uncalled_generic_set_template_stays_outside_collection_route() {
        let source = r#"
            func passthrough<T>(value: Set<T>) -> Set<T> { value }

            func main() -> i32 { 42 }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::OutsideProfile
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Legacy
        ));
    }

    #[test]
    fn unsupported_set_facade_candidate_cannot_reenter_legacy_route() {
        let source = r#"
            func bad<T>(value: Set<T>) -> Set<T> { value }

            func main() -> i32 {
                let values: Set<i32> = {1, 2}
                let result = bad(values)
                drop(result)
                0
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("an admitted but unsupported Set facade must fail closed");
        };
        assert!(
            reason.contains("S11 scalar collection candidate"),
            "{reason}"
        );
        assert!(
            reason.contains("canonical MIR construction failed"),
            "{reason}"
        );
    }
}
