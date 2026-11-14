use super::*;

const SOURCE: &str =
    include_str!("../../../tests/fixtures/mir_ordinary_map_owned_get_default.mimi");

fn checked(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("source fixture lexes");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("source fixture parses");
    crate::core::check_program(&file).expect("source fixture typechecks")
}

fn admitted() -> (crate::core::CheckedProgram, reference::MirProgram) {
    let checked = checked(SOURCE);
    let program = reference::MirProgram::from_checked_program(&checked)
        .expect("exact ResultBranchV3 source lowers to executable MIR");
    validate_ordinary_map_owned_get_result_branch_v3_mir_route(&checked, &program)
        .expect("exact Checker/MIR ResultBranchV3 pair passes Core preflight");
    (checked, program)
}

#[test]
fn result_branch_v3_validator_binds_new_and_set_operations_to_both_source_arms() {
    use crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind as Action;
    use crate::core::OrdinaryMapOwnedGetI32StringV1CfgProfileVersion as Profile;

    let (checked, program) = admitted();
    let owner = crate::core::NodeId("function:main".into());
    let function = program.functions().get(&owner).expect("main MIR function");
    let source = checked
        .resource_analysis(&owner)
        .and_then(|analysis| {
            analysis
                .ordinary_map_owned_get_i32_string_v1_cfg_receipt
                .as_ref()
        })
        .expect("Checker-owned owned-get source receipt");
    assert_eq!(source.profile_version, Profile::ResultBranchV3);

    let new_results = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult { result } => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    let set_results = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult { result, .. } => {
                Some(result)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let new_branches = source
        .result_branches
        .iter()
        .filter(|branch| branch.kind == Action::New)
        .collect::<Vec<_>>();
    let set_branches = source
        .result_branches
        .iter()
        .filter(|branch| branch.kind == Action::Set)
        .collect::<Vec<_>>();
    assert_eq!(new_results.len(), 1);
    assert_eq!(set_results.len(), 2);
    assert_eq!(new_branches.len(), new_results.len());
    assert_eq!(set_branches.len(), set_results.len());

    for branch in new_branches.into_iter().chain(set_branches) {
        let result = MirValueId::new(format!("expr:{}", branch.result_node.0))
            .expect("checked Result node has a canonical MIR value identity");
        assert!(if branch.kind == Action::New {
            new_results.contains(&&result)
        } else {
            set_results.contains(&&result)
        });
        assert_eq!(
            branch.err_payload_type,
            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        );
        assert_eq!(branch.ok.join, branch.err.join);
        assert_eq!(branch.ok.roots_at_exit, branch.err.roots_at_exit);
        assert!(!branch.ok.exit_edges.is_empty());
        assert!(!branch.err.exit_edges.is_empty());
    }

    program
        .type_catalog()
        .validate_ordinary_map_owned_get_i32_string_v1_root_result(
            &types::ordinary_map_owned_get_i32_string_v1_root_result_type_id(),
            &new_branches_source_type(&checked, &owner),
        )
        .expect("synthetic Result TypeDesc is exact Ok(V1 root)/Err(i32)");
}

fn new_branches_source_type(
    checked: &crate::core::CheckedProgram,
    owner: &crate::core::NodeId,
) -> crate::core::ResolvedTypeId {
    checked
        .resource_analysis(owner)
        .and_then(|analysis| {
            analysis
                .ordinary_map_owned_get_i32_string_v1_cfg_receipt
                .as_ref()
        })
        .and_then(|receipt| {
            receipt.result_branches.iter().find(|branch| {
                branch.kind == crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind::New
            })
        })
        .expect("New Result branch source type")
        .result_type
        .clone()
}

#[test]
fn result_branch_v3_validator_rejects_missing_or_forged_source_authority() {
    let (checked_program, program) = admitted();

    let no_source_receipt = checked("func main() -> i32 { 0 }");
    let missing_source =
        validate_ordinary_map_owned_get_result_branch_v3_mir_route(&no_source_receipt, &program)
            .expect_err("an unrelated CheckedProgram cannot authorize receipted MIR");
    assert!(missing_source.contains("no exact ordinary Map owned-get ResultBranchV3 receipt"));

    let owner = crate::core::NodeId("function:main".into());
    let mut missing_mir = program.clone();
    missing_mir
        .function_mut_for_test(&owner)
        .expect("main MIR function")
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt = None;
    assert!(validate_ordinary_map_owned_get_result_branch_v3_mir_route(
        &checked_program,
        &missing_mir
    )
    .is_err());

    let mut forged_mir = program.clone();
    let receipt = forged_mir
        .function_mut_for_test(&owner)
        .expect("main MIR function")
        .ownership
        .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
        .as_mut()
        .expect("MIR carries its source-bound ResultBranchV3 receipt");
    receipt.result_branches[0]
        .err
        .roots_at_entry
        .insert("ordinary-map-owned-get-i32-string-v1:forged".into());
    let forged =
        validate_ordinary_map_owned_get_result_branch_v3_mir_route(&checked_program, &forged_mir)
            .expect_err("MIR-side branch roots cannot be forged independently of Checker");
    assert!(forged.contains("differs from its CheckedProgram source receipt"));
}
