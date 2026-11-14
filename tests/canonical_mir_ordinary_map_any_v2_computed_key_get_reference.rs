use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{
    validate_ordinary_map_owned_any_v2_computed_key_get_mir_route,
    validate_ordinary_map_owned_any_v2_computed_key_get_route, OrdinaryMapOwnedAnyV2Admission,
    ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE, ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
};
use mimi::core::resolved::NodeId;

#[test]
fn computed_key_get_materializes_its_receipt_and_executes_in_reference_mir() {
    let source = include_str!("fixtures/mir_ordinary_map_any_owner_v2_computed_key_get.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("ComputedKeyGet fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("ComputedKeyGet fixture parses");
    let checked =
        mimi::core::check_program(&file).expect("ComputedKeyGet fixture passes the Checker");

    let route = mimi::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("the dedicated ComputedKeyGet profile materializes canonical MIR");
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet
    );
    assert!(route.admission.ordinary_map_owned_any_v2_computed_key_get());
    assert!(
        route.materialized_ordinary_map_owned_any_v2_computed_key_get_candidate,
        "the dedicated ComputedKeyGet receipt must be materialized"
    );
    assert!(
        !route.materialized_ordinary_map_owned_any_v2_candidate,
        "the narrow ComputedKeyGet receipt must not claim broad owner-v2 materialization"
    );
    const {
        assert!(
            !ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the shared owner-v2 production-readiness gate must remain closed"
        );
    };

    validate_ordinary_map_owned_any_v2_computed_key_get_route(&checked, &route.program)
        .expect("the Checker and MIR agree on the exact ComputedKeyGet receipt");
    validate_ordinary_map_owned_any_v2_computed_key_get_mir_route(&route.program)
        .expect("MIR independently validates the exact ComputedKeyGet receipt");

    let receipt = route
        .ordinary_map_owned_any_v2_computed_key_get_route_receipt
        .as_ref()
        .expect("dedicated ComputedKeyGet route receipt");
    assert_eq!(
        receipt.profile,
        ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE
    );
    assert_eq!(
        receipt,
        &route
            .program
            .route_receipt(ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE),
        "the exposed receipt must equal the canonical identity derived from this MIR"
    );

    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(receipt);
    let observation = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("Reference MIR executes the receipted ComputedKeyGet program");
    assert_eq!(observation.value, MirRuntimeValue::Int(0));
    assert_eq!(observation.output, "");
}
