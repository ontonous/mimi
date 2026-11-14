use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{
    materialize_canonical_mir_route,
    validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route,
    validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route,
    validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route,
    CanonicalMirRouteAdmission, OrdinaryMapOwnedAnyV2Admission,
    ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
    ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE,
    ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE,
    ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
};
use mimi::core::resolved::NodeId;

#[test]
fn values_mixed_snapshot_materializes_its_receipt_and_executes_in_reference_mir() {
    let source = include_str!("fixtures/mir_ordinary_map_any_owner_v2_values_mixed_snapshot.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("ValuesMixedSnapshot fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("ValuesMixedSnapshot fixture parses");
    let checked =
        mimi::core::check_program(&file).expect("ValuesMixedSnapshot fixture passes the Checker");

    let route = materialize_canonical_mir_route(&checked, None)
        .expect("the dedicated ValuesMixedSnapshot profile materializes canonical MIR");
    assert_values_mixed_snapshot_admission(route.admission);
    assert!(
        route.materialized_ordinary_map_owned_any_v2_values_mixed_snapshot_candidate,
        "the dedicated ValuesMixedSnapshot receipt must be materialized"
    );
    assert!(
        !route.materialized_ordinary_map_owned_any_v2_candidate,
        "the narrow ValuesMixedSnapshot receipt must not claim broad owner-v2 materialization"
    );
    const {
        assert!(
            !ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the shared owner-v2 production-readiness gate must remain closed"
        );
    };
    validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route(&checked, &route.program)
        .expect("the Checker and MIR agree on the exact ValuesMixedSnapshot receipt");

    let receipt = route
        .ordinary_map_owned_any_v2_values_mixed_snapshot_route_receipt
        .as_ref()
        .expect("dedicated ValuesMixedSnapshot route receipt");
    assert_eq!(
        receipt.profile,
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE
    );

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let observation = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("Reference MIR executes the receipted ValuesMixedSnapshot program");
    assert_eq!(observation.value, MirRuntimeValue::Int(0));
    assert_eq!(observation.output, "true\ntrue\ntrue\ntrue\n");
}

#[test]
fn values_mixed_snapshot_remove_materializes_its_receipt_and_executes_in_reference_mir() {
    let source = include_str!("fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_remove.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("ValuesMixedSnapshotRemove fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("ValuesMixedSnapshotRemove fixture parses");
    let checked = mimi::core::check_program(&file)
        .expect("ValuesMixedSnapshotRemove fixture passes the Checker");

    let route = materialize_canonical_mir_route(&checked, None)
        .expect("the dedicated ValuesMixedSnapshotRemove profile materializes canonical MIR");
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
    );
    assert!(route
        .admission
        .ordinary_map_owned_any_v2_values_mixed_snapshot_remove());
    assert!(
        route.materialized_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_candidate,
        "the dedicated ValuesMixedSnapshotRemove receipt must be materialized"
    );
    assert!(
        !route.materialized_ordinary_map_owned_any_v2_candidate,
        "the narrow ValuesMixedSnapshotRemove receipt must not claim broad owner-v2 materialization"
    );
    const {
        assert!(
            !ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the shared owner-v2 production-readiness gate must remain closed"
        );
    };
    validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(&checked, &route.program)
        .expect("the Checker and MIR agree on the exact ValuesMixedSnapshotRemove receipt");

    let receipt = route
        .ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route_receipt
        .as_ref()
        .expect("dedicated ValuesMixedSnapshotRemove route receipt");
    assert_eq!(
        receipt.profile,
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE
    );

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let observation = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("Reference MIR executes the receipted ValuesMixedSnapshotRemove program");
    assert_eq!(observation.value, MirRuntimeValue::Int(0));
    assert_eq!(observation.output, "true\ntrue\ntrue\ntrue\n");
}

#[test]
fn values_snapshot_replacement_materializes_its_receipt_and_executes_in_reference_mir() {
    let source =
        include_str!("fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_replacement.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("ValuesSnapshotReplacement fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("ValuesSnapshotReplacement fixture parses");
    let checked = mimi::core::check_program(&file)
        .expect("ValuesSnapshotReplacement fixture passes the Checker");

    let route = materialize_canonical_mir_route(&checked, None)
        .expect("the dedicated ValuesSnapshotReplacement profile materializes canonical MIR");
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
    );
    assert!(route
        .admission
        .ordinary_map_owned_any_v2_values_snapshot_replacement());
    assert!(
        !route.admission.ordinary_map_owned_any_v2_complete(),
        "the narrow ValuesSnapshotReplacement admission must not imply CompleteCoverage"
    );
    assert!(
        route.materialized_ordinary_map_owned_any_v2_values_snapshot_replacement_candidate,
        "the dedicated ValuesSnapshotReplacement receipt must be materialized"
    );
    assert!(
        !route.materialized_ordinary_map_owned_any_v2_candidate,
        "the narrow ValuesSnapshotReplacement receipt must not claim broad owner-v2 materialization"
    );
    const {
        assert!(
            !ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the shared owner-v2 production-readiness gate must remain closed"
        );
    };
    validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route(&checked, &route.program)
        .expect("the Checker and MIR agree on the exact ValuesSnapshotReplacement receipt");

    let receipt = route
        .ordinary_map_owned_any_v2_values_snapshot_replacement_route_receipt
        .as_ref()
        .expect("dedicated ValuesSnapshotReplacement route receipt");
    assert_eq!(
        receipt.profile,
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
    );

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let observation = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("Reference MIR executes the receipted ValuesSnapshotReplacement program");
    assert_eq!(observation.value, MirRuntimeValue::Int(0));
    assert_eq!(observation.output, "true\ntrue\n");
}

fn assert_values_mixed_snapshot_admission(admission: CanonicalMirRouteAdmission) {
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshot
    );
    assert!(admission.ordinary_map_owned_any_v2_values_mixed_snapshot());
    assert!(
        !admission.ordinary_map_owned_any_v2_complete(),
        "the narrow ValuesMixedSnapshot admission must not imply CompleteCoverage"
    );
}
