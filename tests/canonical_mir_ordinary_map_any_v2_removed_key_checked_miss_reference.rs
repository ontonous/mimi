use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{
    classify_canonical_mir_route_admission, materialize_canonical_mir_route,
    validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route, CanonicalMirRouteProfile,
    OrdinaryMapOwnedAnyV2Admission, ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
    ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
    ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE,
};
use mimi::core::resolved::NodeId;

const SOURCE: &str =
    include_str!("fixtures/mir_ordinary_map_any_owner_v2_removed_key_checked_miss.mimi");

fn parse_source(source: &str) -> mimi::ast::File {
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("RemovedKeyCheckedMiss source tokenizes");
    mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("RemovedKeyCheckedMiss source parses")
}

fn checked_source(source: &str) -> mimi::core::resolved::CheckedProgram {
    let file = parse_source(source);
    mimi::core::check_program(&file).expect("RemovedKeyCheckedMiss source passes the Checker")
}

fn assert_not_admitted(source: &str, label: &str) {
    let file = parse_source(source);
    match mimi::core::check_program(&file) {
        Ok(checked) => assert_checked_program_not_admitted(&checked, label),
        Err(diagnostics) => assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
            "{label} must either fail closed at the Map/Any Checker boundary or remain outside RemovedKeyCheckedMiss: {diagnostics:?}"
        ),
    }
}

fn assert_checked_program_not_admitted(
    checked: &mimi::core::resolved::CheckedProgram,
    label: &str,
) {
    let admission = classify_canonical_mir_route_admission(checked);
    assert_ne!(
        admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss,
        "{label} received RemovedKeyCheckedMiss Checker authority"
    );

    if let Ok(route) = materialize_canonical_mir_route(checked, None) {
        assert_ne!(
            route.admission.ordinary_map_owned_any_v2,
            OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss,
            "{label} materialized the RemovedKeyCheckedMiss route"
        );
        assert!(
            !route.materialized_ordinary_map_owned_any_v2_removed_key_checked_miss_candidate,
            "{label} materialized a RemovedKeyCheckedMiss candidate"
        );
        assert!(
            route
                .ordinary_map_owned_any_v2_removed_key_checked_miss_route_receipt
                .is_none(),
            "{label} received a RemovedKeyCheckedMiss route receipt"
        );
    }
}

fn assert_checker_rejects_with_code(source: &str, label: &str, expected_code: &str) {
    let file = parse_source(source);
    let diagnostics = mimi::core::check_program(&file)
        .expect_err("the malformed root settlement must be rejected by the Checker");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| {
                matches!(diagnostic.code.as_deref(), Some(code) if code == expected_code || code == "E0200")
            }),
        "{label} must fail with {expected_code}, or be rejected first by the exact Map/Any receipt boundary (E0200): {diagnostics:?}"
    );
}

#[test]
fn removed_key_checked_miss_executes_in_reference_and_ast_free_bytecode() {
    const {
        assert!(
            !ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow RemovedKeyCheckedMiss profile must leave global V2 readiness closed"
        );
    };

    let checked = checked_source(SOURCE);
    let route = materialize_canonical_mir_route(&checked, None)
        .expect("the exact persistent-remove source materializes as RemovedKeyCheckedMiss");
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::RemovedKeyCheckedMiss
    );
    assert!(
        CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss
            .is_admitted(route.admission)
    );
    assert!(route.materialized_ordinary_map_owned_any_v2_removed_key_checked_miss_candidate);
    assert!(!route.materialized_ordinary_map_owned_any_v2_candidate);

    let receipt = route
        .ordinary_map_owned_any_v2_removed_key_checked_miss_route_receipt
        .as_ref()
        .expect("the exact Checker/MIR pair carries a RemovedKeyCheckedMiss route receipt");
    assert_eq!(
        receipt.profile,
        ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE
    );
    assert_eq!(
        receipt,
        &route
            .program
            .route_receipt(ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE),
        "the route receipt is the canonical identity of the materialized MIR"
    );
    let joined_receipt =
        validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route(&checked, &route.program)
            .expect("the public validator joins Checker, MIR, and exact route receipts");
    assert_eq!(&joined_receipt, receipt);

    let operation_receipt = route
        .program
        .ordinary_map_owned_any_v2_removed_key_checked_miss_receipt()
        .expect("derive the exact RemovedKeyCheckedMiss MIR receipt")
        .expect("the route contains its independent operation receipt");
    assert_eq!(operation_receipt.root_drop_instructions.len(), 3);
    assert!(
        operation_receipt.get_any.validate().is_ok(),
        "the GetAny result receipt validates its explicit settlement"
    );
    assert!(
        operation_receipt.unpack.validate().is_ok(),
        "the unpack pair receipt validates its explicit settlement"
    );

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let observed = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference MIR executes the removed-key miss");
    assert_eq!(observed.value, MirRuntimeValue::Bool(true));
    assert_eq!(observed.output, "");

    let bytecode = mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        &checked,
        &route.program,
    )
    .expect("the exact Checker/MIR route adapter seals Bytecode");
    assert!(
        bytecode.ast.is_none(),
        "RemovedKeyCheckedMiss bytecode is AST-free"
    );
    let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("Bytecode executes the Remove-successor canonical i32 miss"),
        mimi::interp::Value::Bool(true)
    );
    assert_eq!(vm.take_stdout(), "");
}

#[test]
fn removed_key_checked_miss_rejects_pre_remove_root_and_wrong_keys() {
    let pre_remove_root = SOURCE.replace(
        "map_get(remove_root, \"removed-key\")",
        "map_get(set_root, \"removed-key\")",
    );
    assert_ne!(
        pre_remove_root, SOURCE,
        "pre-remove-root mutation was applied"
    );
    let pre_remove_file = parse_source(&pre_remove_root);
    match mimi::core::check_program(&pre_remove_file) {
        Ok(pre_remove_checked) => {
            assert_checked_program_not_admitted(
                &pre_remove_checked,
                "GetAny from the pre-Remove Set root",
            );
            let pre_remove_mir =
                mimi::core::mir::reference::MirProgram::from_checked_program(&pre_remove_checked)
                    .expect("the pre-Remove control lowers to diagnostic canonical MIR");
            let pre_remove_route_error =
                validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                    &pre_remove_checked,
                    &pre_remove_mir,
                )
                .expect_err("the pre-Remove hit cannot validate as a checked miss");
            assert!(
                pre_remove_route_error.contains("RemovedKeyCheckedMiss"),
                "the pre-Remove route rejection should identify the narrow profile: {pre_remove_route_error}"
            );
            let pre_remove_adapter_error =
                mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                    &pre_remove_checked,
                    &pre_remove_mir,
                )
                .expect_err("the exact Bytecode adapter cannot seal a pre-Remove hit as a miss");
            assert!(
                format!("{pre_remove_adapter_error:?}").contains("RemovedKeyCheckedMiss"),
                "the pre-Remove adapter rejection should identify the narrow profile: {pre_remove_adapter_error:?}"
            );
            match pre_remove_mir.ordinary_map_owned_any_v2_unpack_result_receipts() {
                Ok(unpacks) => {
                    assert_eq!(unpacks.len(), 1, "the pre-Remove control has one unpack");
                    assert_ne!(
                        unpacks[0].payload_source,
                        mimi::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss,
                        "an i32 target on a String hit in the old Set root is not a canonical miss"
                    );
                }
                Err(_) => {
                    // A source/MIR provenance mismatch may reject this diagnostic
                    // receipt outright. That is fail-closed and cannot authorize a
                    // canonical miss on the old root.
                }
            }
        }
        Err(diagnostics) => assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
            "the pre-Remove String hit must fail closed as an invalid i32 unpack when rejected by the Checker: {diagnostics:?}"
        ),
    }

    let pre_remove_hit_only = pre_remove_root
        .replace("    let miss = unpack_i32(pair.1)\n", "")
        .replace(
            "    let observed = !found && miss == 0",
            "    let observed = found",
        );
    assert_ne!(pre_remove_hit_only, pre_remove_root);
    let hit_checked = checked_source(&pre_remove_hit_only);
    let hit_mir = mimi::core::mir::reference::MirProgram::from_checked_program(&hit_checked)
        .expect("the pre-Remove hit-only control lowers to diagnostic canonical MIR");
    let hit = MirReferenceInterpreter::new(&hit_mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("the old Set root remains queryable as a hit");
    assert_eq!(hit.value, MirRuntimeValue::Bool(true));
    assert_eq!(hit.output, "");

    let wrong_remove_key = SOURCE
        .replace("\"persistent String payload λ\"", "41")
        .replace(
            "map_remove(set_root, \"removed-key\")",
            "map_remove(set_root, \"different-key\")",
        );
    assert_ne!(
        wrong_remove_key, SOURCE,
        "wrong Remove key mutation was applied"
    );
    assert_not_admitted(&wrong_remove_key, "Remove with a different key");

    let wrong_get_key = SOURCE.replace(
        "map_get(remove_root, \"removed-key\")",
        "map_get(remove_root, \"different-key\")",
    );
    assert_ne!(
        wrong_get_key, SOURCE,
        "wrong GetAny key mutation was applied"
    );
    assert_not_admitted(&wrong_get_key, "GetAny with a different key");

    for (expression, label) in [
        ("!found", "found-only formula"),
        ("miss == 0", "payload-only formula"),
        ("!found && miss == 1", "wrong-zero formula"),
        (
            "(!found) == (miss == 0)",
            "equivalence formula with the old equality bug",
        ),
    ] {
        let changed = SOURCE.replace(
            "let observed = !found && miss == 0",
            &format!("let observed = {expression}"),
        );
        assert_ne!(changed, SOURCE, "{label} mutation was applied");
        assert_not_admitted(&changed, label);
    }

    let hit_nonzero = SOURCE
        .replace(
            "    let remove_root = map_remove(set_root, \"removed-key\")\n",
            "",
        )
        .replace(
            "    let pair = map_get(remove_root, \"removed-key\")",
            "    let pair = map_get(set_root, \"removed-key\")",
        )
        .replace("    drop(remove_root)\n", "")
        .replace("\"persistent String payload λ\"", "41")
        .replace(
            "let observed = !found && miss == 0",
            "let observed = (!found) == (miss == 0)",
        );
    assert_ne!(hit_nonzero, SOURCE, "hit/nonzero mutation was applied");
    assert_not_admitted(
        &hit_nonzero,
        "a direct Set-root i32 hit with nonzero payload and the old equality formula",
    );

    let hit_checked = checked_source(&hit_nonzero);
    let hit_mir = mimi::core::mir::reference::MirProgram::from_checked_program(&hit_checked)
        .expect("the hit/nonzero near-miss lowers to diagnostic canonical MIR");
    let hit_receipts = hit_mir
        .ordinary_map_owned_any_v2_unpack_result_receipts()
        .expect("a present selected i32 payload has a source-backed unpack receipt");
    assert_eq!(hit_receipts.len(), 1);
    assert_eq!(
        hit_receipts[0].payload_source,
        mimi::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload,
        "found=true with a nonzero selected payload cannot be relabeled as an i32 miss"
    );
    assert_eq!(
        MirReferenceInterpreter::new(&hit_mir)
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect("the i32 hit with the old equality formula executes"),
        mimi::core::mir::reference::MirExecutionObservation {
            value: MirRuntimeValue::Bool(true),
            output: String::new(),
        }
    );

    let extra_operation = SOURCE
        .replace(
            "let remove_root = map_remove(set_root, \"removed-key\")",
            "let extra_root = map_set(set_root, \"other-key\", \"extra payload\")\n    let remove_root = map_remove(set_root, \"removed-key\")",
        )
        .replace("    drop(set_root)\n", "    drop(set_root)\n    drop(extra_root)\n");
    assert_ne!(
        extra_operation, SOURCE,
        "extra Map operation mutation was applied"
    );
    assert_not_admitted(&extra_operation, "an extra Map Set operation and root");

    let aliased_remove_root = SOURCE
        .replace(
            "let pair = map_get(remove_root, \"removed-key\")",
            "let alias_root = remove_root\n    let pair = map_get(alias_root, \"removed-key\")",
        )
        .replace("    drop(remove_root)\n", "    drop(alias_root)\n");
    assert_ne!(
        aliased_remove_root, SOURCE,
        "Remove-successor alias mutation was applied"
    );
    assert_not_admitted(
        &aliased_remove_root,
        "GetAny through an alias of the Remove successor",
    );

    let missing_root_drop = SOURCE.replace("    drop(empty)\n", "");
    assert_ne!(
        missing_root_drop, SOURCE,
        "missing root Drop mutation was applied"
    );
    assert_checker_rejects_with_code(&missing_root_drop, "missing New-root Drop", "E0256");

    let duplicate_root_drop = SOURCE.replace(
        "    drop(set_root)\n",
        "    drop(set_root)\n    drop(set_root)\n",
    );
    assert_ne!(
        duplicate_root_drop, SOURCE,
        "duplicate root Drop mutation was applied"
    );
    assert_checker_rejects_with_code(&duplicate_root_drop, "duplicate Set-root Drop", "E0304");

    let wrong_root_drop = SOURCE.replace("    drop(set_root)\n", "    drop(remove_root)\n");
    assert_ne!(
        wrong_root_drop, SOURCE,
        "wrong-root Drop mutation was applied"
    );
    let wrong_root_file = parse_source(&wrong_root_drop);
    let wrong_root_diagnostics = mimi::core::check_program(&wrong_root_file)
        .expect_err("a wrong-root Drop must fail closed in the Checker");
    assert!(
        wrong_root_diagnostics.iter().any(|diagnostic| {
            matches!(diagnostic.code.as_deref(), Some("E0200" | "E0256" | "E0304"))
        }),
        "wrong-root settlement must report a linear leak or reuse diagnostic: {wrong_root_diagnostics:?}"
    );
}

#[test]
fn removed_key_checked_miss_rejects_malformed_and_cross_profile_receipts() {
    const {
        assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    let checked = checked_source(SOURCE);
    let route = materialize_canonical_mir_route(&checked, None)
        .expect("the positive RemovedKeyCheckedMiss fixture materializes");
    let profile_receipt = route
        .program
        .route_receipt(ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE);
    let original_digest = route.program.canonical_digest();

    let mut malformed = profile_receipt.clone();
    malformed.mir_digest.push('0');
    assert!(
        malformed.validate_against_program(&route.program).is_err(),
        "a route receipt with a stale MIR digest cannot validate against the program"
    );
    let malformed_error =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&route.program, &malformed)
            .expect_err("a malformed route receipt cannot emit RemovedKeyCheckedMiss bytecode");
    assert!(
        format!("{malformed_error:?}").contains("RemovedKeyCheckedMiss"),
        "malformed receipt rejection must identify the narrow route: {malformed_error:?}"
    );

    let operation_receipt = route
        .program
        .ordinary_map_owned_any_v2_removed_key_checked_miss_receipt()
        .expect("derive the exact RemovedKeyCheckedMiss MIR receipt")
        .expect("the positive route has an operation receipt");
    operation_receipt
        .validate_against_program(&route.program)
        .expect("the unmodified operation receipt validates");

    let mut wrong_owner = operation_receipt.clone();
    wrong_owner.owner = NodeId("function:forged-owner".into());
    assert!(
        wrong_owner
            .validate_against_program(&route.program)
            .is_err(),
        "a RemovedKeyCheckedMiss receipt from another owner cannot validate"
    );

    let mut wrong_key = operation_receipt.clone();
    wrong_key.key_digest.push('0');
    assert!(
        wrong_key.validate_against_program(&route.program).is_err(),
        "a receipt with another key identity cannot validate"
    );

    let mut wrong_type = operation_receipt.clone();
    let mut resolved_types = mimi::core::ir::ResolvedTypeTable::new();
    wrong_type.unpack.conversion_to = resolved_types
        .intern_resolved(mimi::core::ir::ResolvedType::Primitive(
            mimi::core::PrimitiveType::String,
        ))
        .expect("intern a public resolved String type for the negative mutation");
    assert!(
        wrong_type.validate_against_program(&route.program).is_err(),
        "a String conversion target cannot replace the canonical i32 miss target"
    );

    let mut wrong_mir_digest = operation_receipt.clone();
    wrong_mir_digest.mir_digest.push('0');
    assert!(
        wrong_mir_digest
            .validate_against_program(&route.program)
            .is_err(),
        "a receipt bound to another MIR graph cannot validate"
    );

    let generic_receipt = route.program.route_receipt("run-canonical-v1");
    let generic_result = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
        &route.program,
        &generic_receipt,
    );
    match generic_result {
        Err(errors) => {
            assert!(
            errors.iter().any(|error| error.message.contains("RemovedKeyCheckedMiss")),
            "generic receipt rejection should identify the missing narrow authority: {errors:?}"
        )
        }
        Ok(bytecode) => {
            assert!(bytecode.ast.is_none());
            let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
            let error = vm
                .run_value()
                .expect_err("a generic receipt cannot authorize RemovedKeyCheckedMiss execution");
            assert!(
                error.message().contains("diagnostic-only")
                    || error.message().contains("RemovedKeyCheckedMiss"),
                "generic receipt failure should identify its closed authority: {error}"
            );
            assert_eq!(vm.take_stdout(), "");
        }
    }

    let cross_profile = route
        .program
        .route_receipt(ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE);
    let cross_profile_error = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
        &route.program,
        &cross_profile,
    )
    .expect_err("a CheckedGet profile receipt cannot authorize RemovedKeyCheckedMiss bytecode");
    assert!(
        format!("{cross_profile_error:?}").contains("CheckedGet"),
        "cross-profile rejection must identify the foreign profile: {cross_profile_error:?}"
    );
    assert_eq!(route.program.canonical_digest(), original_digest);
    const {
        assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
}
