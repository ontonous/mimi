use super::checked_values_program;
use crate::core::NodeId;

#[test]
fn ordinary_map_owned_any_v2_values_producer_withholds_authority_for_unlowered_declaration_root() {
    let producer_owner = NodeId("function:snapshot_values".into());
    let checked = checked_values_program(
        r#"func direct_caller() -> i32 {
    let snapshot = snapshot_values()
    let count = snapshot.len()
    drop(snapshot)
    count
}"#,
        r#"const hidden_snapshot = f"hidden {snapshot_values()} {sqrt(value = 1)}""#,
        "direct_caller()",
    );

    let declaration = checked
        .declaration_expression_bodies()
        .values()
        .find(|root| root.owner == NodeId("constant:hidden_snapshot".into()))
        .expect("the f-string initializer has a Checker-owned declaration root");
    let lowering_errors = declaration
        .body
        .as_ref()
        .expect_err("the f-string root must remain deferred");
    assert!(
        lowering_errors
            .iter()
            .any(|error| error.message.contains("named arguments for builtin call")),
        "the otherwise lowerable Values interpolation precedes the unsupported named builtin argument: {lowering_errors:?}"
    );

    let producer_receipt = checked
        .resource_analysis(&producer_owner)
        .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
        .expect("the producer retains its V2 CFG receipt");
    assert!(
        producer_receipt.values_producer_return_receipt.is_some(),
        "the producer-local Values return proof remains valid"
    );
    assert!(
        producer_receipt.values_producer_caller_receipt.is_none(),
        "an unlowered declaration root must withhold cross-call producer authority"
    );

    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage,
        "the unproven declaration root keeps producer-caller source intent fail-closed"
    );
    assert!(matches!(
        crate::core::mir::materialize_canonical_mir_route(&checked, None),
        Err(crate::core::mir::CanonicalMirRouteMaterializationError::Complete {
            profile: crate::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2,
            stage: crate::core::mir::CanonicalMirRouteFailureStage::Coverage,
            ..
        })
    ));
}
