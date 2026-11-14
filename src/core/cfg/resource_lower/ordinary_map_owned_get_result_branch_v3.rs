use super::*;

#[test]
fn source_receipt_records_new_set_result_arms_and_edge_local_root_duties() {
    let file = parse(include_str!(
        "../../../../tests/fixtures/mir_ordinary_map_owned_get_default.mimi"
    ));
    let checked = crate::core::check_program(&file)
        .expect("ordinary owned-get ResultBranchV3 source fixture checks");
    let analysis = checked
        .resource_analysis(&NodeId("function:main".into()))
        .expect("main resource analysis");
    let receipt = analysis
        .ordinary_map_owned_get_i32_string_v1_cfg_receipt
        .as_ref()
        .expect("exact typed owned-get CFG receipt");

    assert_eq!(
        receipt.profile_version,
        crate::core::OrdinaryMapOwnedGetI32StringV1CfgProfileVersion::ResultBranchV3
    );
    assert_eq!(
        receipt
            .result_branches
            .iter()
            .filter(
                |branch| branch.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::New
            )
            .count(),
        1,
        "the exact fixture has one fallible New operation"
    );
    assert_eq!(
        receipt
            .result_branches
            .iter()
            .filter(
                |branch| branch.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Set
            )
            .count(),
        2,
        "the exact fixture has two fallible Set operations"
    );

    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    for branch in &receipt.result_branches {
        assert_eq!(branch.err_payload_type, i32_type);
        assert_eq!(branch.ok.join, branch.err.join);
        assert!(!branch.ok.exit_edges.is_empty());
        assert!(!branch.err.exit_edges.is_empty());
        assert_eq!(branch.ok.roots_at_exit, branch.err.roots_at_exit);

        for (path, variant) in [
            (&branch.ok, &branch.ok_variant),
            (&branch.err, &branch.err_variant),
        ] {
            let entry = receipt
                .reachable_edges
                .get(&path.arm_entry_edge)
                .expect("Result arm entry edge is present");
            assert_eq!(
                entry.source.node,
                if variant.0.ends_with("::Ok") {
                    branch.ok_arm.clone()
                } else {
                    branch.err_arm.clone()
                }
            );
            assert!(path.path_edges.contains(&path.arm_entry_edge));
            for edge_id in &path.exit_edges {
                let edge = receipt
                    .reachable_edges
                    .get(edge_id)
                    .expect("branch exit edge is present");
                assert_eq!(edge.to, path.join);
            }
        }

        let root_identity = |local: &crate::core::ResolvedLocalId| {
            format!("ordinary-map-owned-get-i32-string-v1:{}", local.0 .0)
        };
        match branch.kind {
            crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::New => {
                let output = root_identity(branch.output_root.as_ref().expect("New Ok root"));
                assert!(branch.ok.roots_at_entry.contains(&output));
                assert!(!branch.err.roots_at_entry.contains(&output));
                assert!(!branch.err.roots_at_exit.contains(&output));
                assert!(branch.input_root.is_none());
            }
            crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Set => {
                let input_local = branch.input_root.as_ref().expect("Set source root");
                let input = root_identity(input_local);
                let output = root_identity(branch.output_root.as_ref().expect("Set Ok root"));
                assert!(branch.ok.roots_at_entry.contains(&input));
                assert!(branch.err.roots_at_entry.contains(&input));
                assert!(branch.ok.roots_at_entry.contains(&output));
                assert!(!branch.err.roots_at_entry.contains(&output));
                assert!(!branch.err.roots_at_exit.contains(&input));
                assert!(
                    analysis
                        .ordinary_map_owned_get_i32_string_v1_actions
                        .iter()
                        .any(|action| {
                            action.kind
                                == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::Drop
                                && action.local == *input_local
                                && branch.err.explicit_root_drops.contains(&action.point)
                        }),
                    "Set Err must explicitly discharge its carried source root before the join"
                );
            }
            _ => {}
        }
    }
}
