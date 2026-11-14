use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{classify_canonical_mir_route_admission, materialize_canonical_mir_route};
use mimi::core::resolved::{CheckedProgram, NodeId};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "support/memcheck_summary.rs"]
mod strict_memcheck_summary;

// CLI census exposes the stable textual identity; the Checker/MIR assertions
// below independently bind it to the typed profile and receipt.
const ROUTE_PROFILE: &str = "ordinary-map-owned-any-v2-get-or-default-string-v1";
const ROUTE_ADMISSION_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const NATIVE_BUILD_PREFIX: &str = "MIMI_ROUTE_CENSUS canonical-native-build ";
const VERIFIER_PROVENANCE_PREFIX: &str = "canonical MIR verifier provenance: ";
const ACTOR_SHARED_HELPER_FIXTURE_SHA256: &str =
    "75f7aa4bfc0e7b0b4f447dd64abb6b068f6a6e7f25f66d9a09f49d0549cd36ac";
const EXPECTED_HIT_STDOUT: &[u8] = b"stored hit\n";
const EXPECTED_EMPTY_HIT_STDOUT: &[u8] = b"\n";
const EXPECTED_MISS_STDOUT: &[u8] = b"default miss\n";
const EXPECTED_HELPER_STDOUT: &[u8] = b"helper hit\n";

struct CheckedFixture {
    checked: CheckedProgram,
    excluded_sources: HashSet<mimi::span::SourceId>,
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn assert_fixture_sha256(name: &str, expected: &str) {
    let path = fixture_path(name);
    let output = Command::new("sha256sum")
        .arg(&path)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "hash Actor shared-helper fixture {}: {error}",
                path.display()
            )
        });
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        output_text(&output.stderr)
    );
    let stdout = output_text(&output.stdout);
    let digest = stdout
        .split_whitespace()
        .next()
        .expect("sha256sum output contains the fixture digest");
    assert_eq!(
        digest,
        expected,
        "Actor shared-helper fixture bytes changed: {}",
        path.display()
    );
}

fn checked_fixture(name: &str) -> Result<CheckedFixture, Vec<mimi::diagnostic::Diagnostic>> {
    let path = fixture_path(name);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read get_or_default fixture {}: {error}", path.display()));
    let tokens = mimi::lexer::Lexer::new(&source)
        .tokenize()
        .unwrap_or_else(|error| {
            panic!(
                "get_or_default fixture {} tokenizes: {error:?}",
                path.display()
            )
        });
    let parsed = mimi::loader::parser_for_path(tokens, &path)
        .unwrap_or_else(|error| {
            panic!(
                "get_or_default fixture {} parser setup: {error}",
                path.display()
            )
        })
        .parse_file()
        .unwrap_or_else(|error| {
            panic!(
                "get_or_default fixture {} parses: {error:?}",
                path.display()
            )
        });

    let mut file = if parsed.imports.is_empty() {
        parsed
    } else {
        let base_dir = path
            .parent()
            .expect("get_or_default fixture has a parent directory")
            .to_path_buf();
        let mut loader = mimi::loader::ModuleLoader::new(base_dir);
        loader
            .load_main_with_file(&path, parsed)
            .unwrap_or_else(|error| panic!("load imports for {}: {error}", path.display()));
        loader
            .merge_all()
            .unwrap_or_else(|error| panic!("merge imports for {}: {error}", path.display()))
    };
    mimi::loader::merge_prelude_into(&mut file);
    let excluded_sources = file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect();

    mimi::core::check_program(&file).map(|checked| CheckedFixture {
        checked,
        excluded_sources,
    })
}

fn checked_inline_source(
    source: &str,
) -> Result<CheckedFixture, Vec<mimi::diagnostic::Diagnostic>> {
    // Reuse a real fixture path for module resolution, while parsing the
    // caller-provided source bytes. This keeps std::maps loading identical to
    // the checked fixtures without adding another on-disk source fixture.
    let path = fixture_path("mir_ordinary_map_owned_any_v2_get_or_default_string_hit.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("inline String census source tokenizes");
    let parsed = mimi::loader::parser_for_path(tokens, &path)
        .expect("inline String census source parser setup")
        .parse_file()
        .expect("inline String census source parses");
    let mut file = if parsed.imports.is_empty() {
        parsed
    } else {
        let mut loader = mimi::loader::ModuleLoader::new(
            path.parent()
                .expect("String fixture path has a parent")
                .to_path_buf(),
        );
        loader
            .load_main_with_file(&path, parsed)
            .expect("load std::maps for inline String census source");
        loader
            .merge_all()
            .expect("merge std::maps for inline String census source")
    };
    mimi::loader::merge_prelude_into(&mut file);
    let excluded_sources = file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect();
    mimi::core::check_program(&file).map(|checked| CheckedFixture {
        checked,
        excluded_sources,
    })
}

fn route_for(
    fixture: &CheckedFixture,
    label: &str,
) -> mimi::core::mir::CanonicalMirRouteMaterialization {
    materialize_canonical_mir_route(&fixture.checked, Some(&fixture.excluded_sources))
        .unwrap_or_else(|error| panic!("{label} canonical MIR materializes: {error}"))
}

fn assert_get_or_default_string_profile(
    fixture: &CheckedFixture,
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
    label: &str,
    expected_owner: &str,
) {
    let profile =
        mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultString;
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
        "{label} must use the independent GetOrDefaultString admission, not CheckedGet"
    );
    assert!(
        profile.is_admitted(route.admission),
        "{label} typed profile admission"
    );
    assert!(
        profile.is_materialized(route),
        "{label} typed profile MIR materialization"
    );
    assert!(
        !route.materialized_ordinary_map_owned_any_v2_candidate,
        "{label} must leave broad Ordinary Map/Any V2 readiness closed"
    );
    let receipt = route.program.route_receipt(ROUTE_PROFILE);
    assert_eq!(
        receipt.profile, ROUTE_PROFILE,
        "{label} route receipt profile"
    );

    let receipts = route
        .program
        .ordinary_map_owned_any_v2_get_or_default_string_receipts()
        .expect("GetOrDefault<String> MIR receipt derives");
    let [mir_receipt] = receipts.as_slice() else {
        panic!("{label} must have exactly one typed GetOrDefault<String> MIR receipt");
    };
    assert!(
        std::any::type_name_of_val(mir_receipt)
            .ends_with("OrdinaryMapOwnedAnyV2GetOrDefaultStringMirReceipt"),
        "{label} must derive the dedicated String MIR receipt type"
    );
    assert_eq!(mir_receipt.profile, ROUTE_PROFILE, "{label} MIR profile");
    mir_receipt
        .validate_against_program(&route.program)
        .unwrap_or_else(|error| panic!("{label} GetOrDefault<String> receipt validates: {error}"));

    assert_eq!(
        mir_receipt.owner,
        NodeId(expected_owner.into()),
        "{label} exact caller owner"
    );
    assert_ne!(
        mir_receipt.owner, mir_receipt.call_target,
        "{label} helper is a distinct callable"
    );
    let callsite = fixture
        .checked
        .call_sites()
        .get(&mir_receipt.callsite)
        .unwrap_or_else(|| panic!("{label} Checker callsite is present"));
    assert_eq!(
        callsite.target_function.as_ref(),
        Some(&mir_receipt.call_target)
    );
    assert_eq!(
        callsite.resolved_callee,
        Some(mimi::core::ResolvedCallee::Function(
            mir_receipt.call_target.clone()
        )),
        "{label} exact Checker callee identity"
    );
    assert_eq!(
        mir_receipt.caller_root_argument, mir_receipt.argument_order[0],
        "{label} first Checker argument is the caller-owned Map root"
    );
    assert_eq!(
        mir_receipt.call_parameter_order[0], mir_receipt.helper_map_parameter,
        "{label} caller Map argument is passed to the canonical helper's Map parameter"
    );
    assert_eq!(
        mir_receipt.call_parameter_order[1], mir_receipt.helper_key_parameter,
        "{label} query key is passed to the canonical helper's key parameter"
    );
    assert_eq!(
        mir_receipt.call_parameter_order[2], mir_receipt.helper_default_parameter,
        "{label} caller default is passed to the canonical helper's default parameter"
    );
    assert_eq!(
        mir_receipt.helper_map_permission,
        mimi::core::Permission::View,
        "{label} helper borrows the caller Map root through a View parameter"
    );
    assert_ne!(
        mir_receipt.caller_root_local, mir_receipt.caller_initial_root_local,
        "{label} root lineage uses distinct locals only after persistent Set"
    );

    let catalog = route.program.type_catalog();
    let root_desc = catalog
        .get(&mir_receipt.root_type)
        .unwrap_or_else(|| panic!("{label} root TypeDesc exists"));
    assert_eq!(
        root_desc.kind,
        mimi::core::mir::types::MirTypeKind::OrdinaryMapOwnedAnyV2,
        "{label} root operand has the exact V2 Map TypeDesc"
    );
    assert_eq!(
        root_desc.layout,
        mimi::core::mir::types::MirLayout::OrdinaryMapOwnedAnyV2,
        "{label} root operand has the exact opaque Map layout"
    );
    assert_eq!(
        root_desc.ownership,
        mimi::core::mir::types::MirOwnership::Move
    );
    for (role, ty, expected_primitive) in [
        (
            "key",
            &mir_receipt.key_type,
            mimi::core::PrimitiveType::String,
        ),
        (
            "default",
            &mir_receipt.default_type,
            mimi::core::PrimitiveType::String,
        ),
        (
            "result",
            &mir_receipt.result_type,
            mimi::core::PrimitiveType::String,
        ),
    ] {
        let desc = catalog
            .get(ty)
            .unwrap_or_else(|| panic!("{label} {role} TypeDesc exists"));
        assert_eq!(
            desc.kind,
            mimi::core::mir::types::MirTypeKind::Primitive(expected_primitive),
            "{label} {role} operand has the exact primitive TypeDesc"
        );
        let expected_layout = if expected_primitive == mimi::core::PrimitiveType::String {
            mimi::core::mir::types::MirLayout::Handle
        } else {
            mimi::core::mir::types::MirLayout::Scalar
        };
        let expected_ownership = if expected_primitive == mimi::core::PrimitiveType::String {
            mimi::core::mir::types::MirOwnership::Move
        } else {
            mimi::core::mir::types::MirOwnership::Copy
        };
        assert_eq!(desc.layout, expected_layout, "{label} {role} MIR layout");
        assert_eq!(
            desc.ownership, expected_ownership,
            "{label} {role} ownership"
        );
    }
    assert_eq!(
        mir_receipt.type_argument, mir_receipt.default_type,
        "{label} specialization is exactly the default's String TypeDesc"
    );
    assert_eq!(
        mir_receipt.type_argument, mir_receipt.result_type,
        "{label} specialization is exactly the result's String TypeDesc"
    );
    assert!(
        !mir_receipt.helper_pair_drop_point.0.trim().is_empty(),
        "{label} exact route receipt names the helper's explicit GetAny pair Drop"
    );
    assert!(
        !mir_receipt.helper_unpack_cast.0.trim().is_empty(),
        "{label} exact route receipt names the found-arm String unpack"
    );
    assert!(
        !mir_receipt.helper_default_pack.0.trim().is_empty(),
        "{label} exact route receipt names the default's DynamicAny pack"
    );
    assert_eq!(
        mir_receipt.helper_default_pack_from, mir_receipt.default_type,
        "{label} default pack starts from the caller's exact String TypeDesc"
    );
    assert_ne!(
        mir_receipt.helper_default_pack_to, mir_receipt.default_type,
        "{label} default pack enters the helper's DynamicAny join"
    );
    let packed_default_desc = catalog
        .get(&mir_receipt.helper_default_pack_to)
        .unwrap_or_else(|| panic!("{label} helper default DynamicAny TypeDesc exists"));
    assert_eq!(
        packed_default_desc.kind,
        mimi::core::mir::types::MirTypeKind::DynamicAny,
        "{label} resolved default pack enters the helper's DynamicAny join"
    );
    let selected_payload_desc = catalog
        .get(&mir_receipt.selected_payload_type_desc)
        .unwrap_or_else(|| panic!("{label} selected owned String TypeDesc exists"));
    assert_eq!(
        selected_payload_desc.kind,
        mimi::core::mir::types::MirTypeKind::Primitive(mimi::core::PrimitiveType::String),
        "{label} found payload and miss default both rejoin as String"
    );
    assert_eq!(
        selected_payload_desc.layout,
        mimi::core::mir::types::MirLayout::Handle,
        "{label} selected String result uses the canonical MIR handle layout"
    );
    assert_eq!(
        selected_payload_desc.ownership,
        mimi::core::mir::types::MirOwnership::Move,
        "{label} selected String is moved to the caller result"
    );
    catalog
        .validate_owned_string(&mir_receipt.selected_payload_type_desc)
        .unwrap_or_else(|error| panic!("{label} selected String glue validates: {error}"));
    let miss_payload_desc = catalog
        .get(&mir_receipt.miss_payload_type_desc)
        .unwrap_or_else(|| panic!("{label} miss sentinel TypeDesc exists"));
    assert_eq!(
        miss_payload_desc.kind,
        mimi::core::mir::types::MirTypeKind::Primitive(mimi::core::PrimitiveType::I32),
        "{label} a miss remains the exact Copy i32 sentinel, never decoded as String"
    );
    assert_eq!(
        miss_payload_desc.layout,
        mimi::core::mir::types::MirLayout::Scalar,
        "{label} miss sentinel ABI is scalar"
    );
    assert_eq!(
        miss_payload_desc.ownership,
        mimi::core::mir::types::MirOwnership::Copy,
        "{label} miss sentinel ownership is Copy"
    );
    assert_digest(
        &mir_receipt.miss_payload_type_desc_digest,
        &format!("{label} canonical miss sentinel digest"),
    );

    let caller = route
        .program
        .functions()
        .get(&mir_receipt.owner)
        .unwrap_or_else(|| panic!("{label} caller MIR function exists"));
    let mut operation_sites = Vec::new();
    let mut initial_root_drops = Vec::new();
    let mut root_drops = Vec::new();
    let mut set_root_drops = Vec::new();
    let initial_root_value = mimi::core::mir::MirValueId::new(format!(
        "local:{}",
        mir_receipt.caller_initial_root_local.0 .0
    ))
    .expect("Checker root local has a valid MIR value identity");
    let set_root_value = mimi::core::mir::MirValueId::new(format!(
        "local:{}",
        mir_receipt.caller_set_root_local.0 .0
    ))
    .expect("Checker Set-result root local has a valid MIR value identity");
    for (block_id, block) in &caller.blocks {
        for (index, instruction) in block.instructions.iter().enumerate() {
            if instruction.id == mir_receipt.instruction {
                operation_sites.push((block_id, index, instruction));
            }
            if let mimi::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } =
                &instruction.kind
            {
                if root == &initial_root_value {
                    initial_root_drops.push((block_id, index, instruction));
                }
                if root == &mir_receipt.root {
                    root_drops.push((block_id, index, instruction));
                }
                if root == &set_root_value {
                    set_root_drops.push((block_id, index, instruction));
                }
            }
        }
    }
    let [(operation_block, operation_index, operation)] = operation_sites.as_slice() else {
        panic!("{label} has exactly one MIR operation at the receipted callsite");
    };
    match &operation.kind {
        mimi::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString {
            result,
            root,
            key,
            default,
            callsite,
            call_target,
            type_argument,
        } => {
            assert_eq!(result, &mir_receipt.result, "{label} result binding");
            assert_eq!(root, &mir_receipt.root, "{label} borrowed root operand");
            assert_eq!(key, &mir_receipt.key, "{label} exact key operand");
            assert_eq!(
                default, &mir_receipt.default,
                "{label} exact default operand"
            );
            assert_eq!(callsite, &mir_receipt.callsite, "{label} source callsite");
            assert_eq!(
                call_target, &mir_receipt.call_target,
                "{label} canonical helper"
            );
            assert_eq!(
                type_argument, &mir_receipt.type_argument,
                "{label} String specialization"
            );
        }
        other => panic!("{label} receipt instruction has the wrong MIR operation: {other:?}"),
    }

    let [(initial_drop_block, initial_drop_index, initial_drop)] = initial_root_drops.as_slice()
    else {
        panic!("{label} has exactly one explicit Drop for the original Map root");
    };
    let [(root_drop_block, root_drop_index, root_drop)] = root_drops.as_slice() else {
        panic!("{label} has exactly one explicit Drop for the caller receiver root");
    };
    let [(set_root_drop_block, set_root_drop_index, set_root_drop)] = set_root_drops.as_slice()
    else {
        panic!("{label} has exactly one explicit Drop for the persistent Set-result root");
    };
    assert_ne!(
        mir_receipt.caller_initial_root_drop_point, mir_receipt.caller_root_drop_point,
        "{label} persistent Map roots have distinct Checker Drop points"
    );
    assert_ne!(
        initial_drop.id, root_drop.id,
        "{label} roots have distinct MIR Drop instructions"
    );
    assert_ne!(
        initial_drop.id, set_root_drop.id,
        "{label} Set-result and initial Map roots have distinct MIR Drop instructions"
    );
    assert!(
        initial_drop.id.as_str().ends_with(&format!(
            ":{}",
            mir_receipt.caller_initial_root_drop_point.0
        )),
        "{label} initial-root Drop instruction matches its Checker point"
    );
    assert!(
        root_drop
            .id
            .as_str()
            .ends_with(&format!(":{}", mir_receipt.caller_root_drop_point.0)),
        "{label} caller receiver Drop instruction matches its Checker point"
    );
    assert!(
        set_root_drop
            .id
            .as_str()
            .ends_with(&format!(":{}", mir_receipt.caller_set_root_drop_point.0)),
        "{label} persistent Set-result Drop instruction matches its Checker point"
    );
    assert_eq!(
        operation_block, initial_drop_block,
        "{label} original-root Drop follows the call in its caller block"
    );
    assert_eq!(
        operation_block, root_drop_block,
        "{label} root Drop follows the call in its caller block"
    );
    assert_eq!(
        operation_block, set_root_drop_block,
        "{label} Set-result source-root Drop follows the call in its caller block"
    );
    assert!(
        initial_drop_index > operation_index,
        "{label} original Map root has a separate explicit Drop after the call"
    );
    assert!(
        root_drop_index > operation_index,
        "{label} borrowed Map root remains caller-owned until its later explicit Drop"
    );
    assert!(
        set_root_drop_index > operation_index,
        "{label} persistent Set-result Map root remains live until its explicit Drop"
    );
    assert_eq!(
        mir_receipt.caller_set_root_local == mir_receipt.caller_root_local,
        mir_receipt.caller_set_root_drop_point == mir_receipt.caller_root_drop_point,
        "{label} no-alias roots share one receipt-bound Drop; alias roots have distinct drops"
    );
    assert_eq!(
        mir_receipt.caller_set_root_drop_place_index, 0,
        "{label} Set-result root Drop selects the sole exact direct place"
    );
    if mir_receipt.caller_set_root_local != mir_receipt.caller_root_local {
        assert_ne!(
            set_root_drop.id, root_drop.id,
            "{label} source and alias own separate Map roots"
        );
        assert_ne!(
            set_root_drop_index, root_drop_index,
            "{label} source and alias settle at distinct MIR instructions"
        );
    }
    assert!(
        root_drop_index > initial_drop_index,
        "{label} original and successor Map roots are dropped separately in source order"
    );
}

fn assert_reference_and_ast_free_bytecode(
    fixture: &CheckedFixture,
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
    expected_stdout: &[u8],
    label: &str,
) {
    let receipt = route.program.route_receipt(ROUTE_PROFILE);
    let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(&receipt);
    let observation = reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} Reference MIR executes: {error}"));
    assert_eq!(
        observation.value,
        MirRuntimeValue::Int(0),
        "{label} return value"
    );
    assert_observed_output(
        &format!("{label}-reference"),
        observation.output.as_bytes(),
        expected_stdout,
    );

    let bytecode = mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        &fixture.checked,
        &route.program,
        &receipt,
    )
    .unwrap_or_else(|error| {
        panic!("{label} CheckedProgram-bound receipt compiles to AST-free Bytecode: {error:?}")
    });
    assert!(
        bytecode.ast.is_none(),
        "{label} bytecode must not retain source AST"
    );
    let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .unwrap_or_else(|error| panic!("{label} Bytecode executes: {error}")),
        mimi::interp::Value::Int(0),
        "{label} Bytecode return value"
    );
    let bytecode_output = vm.take_stdout();
    assert_observed_output(
        &format!("{label}-bytecode"),
        bytecode_output.as_bytes(),
        expected_stdout,
    );
}

fn assert_observed_output(label: &str, observed: &[u8], expected: &[u8]) {
    assert_eq!(observed, expected, "{label} output");
    let bytes_hex = observed
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!("MIMI_GET_OR_DEFAULT_STRING_OUTPUT label={label} bytes_hex={bytes_hex}");
}

#[test]
fn get_or_default_string_hit_empty_hit_and_miss_match_reference_bytecode() {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow GetOrDefaultString profile must not open broad V2 readiness"
        );
    };

    let mut positive_identities = Vec::new();
    for (label, fixture_name, expected_stdout) in [
        (
            "hit",
            "mir_ordinary_map_owned_any_v2_get_or_default_string_hit.mimi",
            EXPECTED_HIT_STDOUT,
        ),
        (
            "hit-empty",
            "mir_ordinary_map_owned_any_v2_get_or_default_string_hit_empty.mimi",
            EXPECTED_EMPTY_HIT_STDOUT,
        ),
        (
            "alias-hit",
            "mir_ordinary_map_owned_any_v2_get_or_default_string_alias.mimi",
            EXPECTED_HIT_STDOUT,
        ),
        (
            "miss",
            "mir_ordinary_map_owned_any_v2_get_or_default_string_miss.mimi",
            EXPECTED_MISS_STDOUT,
        ),
    ] {
        let fixture = checked_fixture(fixture_name).unwrap_or_else(|diagnostics| {
            panic!("{label} fixture passes Checker: {diagnostics:?}")
        });
        let admission = classify_canonical_mir_route_admission(&fixture.checked);
        assert_eq!(
            admission.ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
            "{label} Checker profile is independently classified"
        );
        let route = route_for(&fixture, label);
        assert_eq!(
            admission, route.admission,
            "{label} admission survives MIR materialization"
        );
        assert_get_or_default_string_profile(&fixture, &route, label, "function:main");
        assert_reference_and_ast_free_bytecode(&fixture, &route, expected_stdout, label);
        positive_identities.push(route.program.route_receipt(ROUTE_PROFILE).mir_digest);
    }

    let helper_fixture = checked_fixture(
        "mir_ordinary_map_owned_any_v2_get_or_default_string_reachable_helper.mimi",
    )
    .expect("main-to-ordinary-helper GetOrDefault<String> fixture passes Checker");
    let helper_admission = classify_canonical_mir_route_admission(&helper_fixture.checked);
    assert_eq!(
        helper_admission.ordinary_map_owned_any_v2,
        mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
        "a static main-to-ordinary-helper path keeps the narrow Checker profile"
    );
    let helper_route = route_for(&helper_fixture, "reachable helper");
    assert_eq!(helper_admission, helper_route.admission);
    assert_get_or_default_string_profile(
        &helper_fixture,
        &helper_route,
        "reachable helper",
        "function:lookup",
    );
    assert_reference_and_ast_free_bytecode(
        &helper_fixture,
        &helper_route,
        EXPECTED_HELPER_STDOUT,
        "reachable-helper",
    );
    positive_identities.push(helper_route.program.route_receipt(ROUTE_PROFILE).mir_digest);

    for left in 0..positive_identities.len() {
        for right in left + 1..positive_identities.len() {
            assert_ne!(
                positive_identities[left], positive_identities[right],
                "distinct String hit/miss/helper source shapes retain separate MIR identities"
            );
        }
    }
}

#[test]
fn get_or_default_string_typed_mir_receipt_rejects_checker_binding_forgery() {
    let fixture = checked_fixture("mir_ordinary_map_owned_any_v2_get_or_default_string_hit.mimi")
        .expect("GetOrDefaultString hit fixture passes Checker");
    let route = route_for(&fixture, "GetOrDefaultString receipt-forgery case");
    let route_receipt = route.program.route_receipt(ROUTE_PROFILE);
    let bytecode = mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        &fixture.checked,
        &route.program,
        &route_receipt,
    )
    .expect("the exact CheckedProgram/MIR receipt compiles");
    assert!(bytecode.ast.is_none(), "the admitted bytecode is AST-free");

    let miss_fixture =
        checked_fixture("mir_ordinary_map_owned_any_v2_get_or_default_string_miss.mimi")
            .expect("GetOrDefaultString miss fixture passes Checker");
    let cross_checker = mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        &miss_fixture.checked,
        &route.program,
        &route_receipt,
    )
    .expect_err("a different Checker callsite cannot authorize the hit MIR");
    assert!(
        cross_checker.iter().any(|error| error
            .message
            .contains("GetOrDefault<String> Checker/MIR route rejected")),
        "cross-Checker rejection must happen at the dedicated route boundary: {cross_checker:?}"
    );

    for (label, forged) in {
        let mut wrong_profile = route_receipt.clone();
        wrong_profile.profile = "ordinary-map-owned-any-v2-v1".into();
        let mut wrong_mir_digest = route_receipt.clone();
        wrong_mir_digest.mir_digest = "00".repeat(32);
        let mut wrong_type_desc_digest = route_receipt.clone();
        wrong_type_desc_digest.type_desc_digest = "11".repeat(32);
        let mut wrong_ownership_digest = route_receipt.clone();
        wrong_ownership_digest.ownership_digest = "22".repeat(32);
        let mut wrong_root_owners = route_receipt.clone();
        wrong_root_owners.root_owners = vec![NodeId("forged:root-owner".into())];
        [
            ("wrong route profile", wrong_profile),
            ("wrong MIR digest", wrong_mir_digest),
            ("wrong TypeDesc digest", wrong_type_desc_digest),
            ("wrong ownership digest", wrong_ownership_digest),
            ("wrong root-owner census", wrong_root_owners),
        ]
    } {
        let errors = mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_string_route(
            &fixture.checked,
            &route.program,
            &forged,
        )
        .expect_err("a modified route receipt must be rejected before Bytecode emission");
        assert!(
            errors.iter().any(|error| {
                error
                    .message
                    .contains("supplied GetOrDefault<String> route receipt is not the exact")
            }),
            "{label} must fail exact receipt equality before backend emission: {errors:?}"
        );
    }

    let typed_receipts = route
        .program
        .ordinary_map_owned_any_v2_get_or_default_string_receipts()
        .expect("the exact typed MIR receipt independently derives");
    let [typed_receipt] = typed_receipts.as_slice() else {
        panic!("one exact typed GetOrDefault<String> MIR receipt is expected");
    };
    typed_receipt
        .validate_against_program(&route.program)
        .expect("unmodified typed receipt validates against MIR");

    macro_rules! reject_typed_receipt_mutation {
        ($label:literal, $field:ident, $value:expr) => {{
            let mut forged = (*typed_receipt).clone();
            forged.$field = $value;
            assert_ne!(
                &forged, typed_receipt,
                "test mutation must change the typed MIR receipt: {}",
                $label
            );
            assert!(
                forged.validate_against_program(&route.program).is_err(),
                concat!("typed MIR receipt must reject ", $label)
            );
        }};
    }

    reject_typed_receipt_mutation!(
        "wrong caller",
        owner,
        NodeId("function:forged-caller".into())
    );
    reject_typed_receipt_mutation!("wrong caller root", root, typed_receipt.key.clone());
    reject_typed_receipt_mutation!(
        "wrong caller root argument",
        caller_root_argument,
        NodeId("argument:forged-root".into())
    );
    reject_typed_receipt_mutation!(
        "wrong helper formal parameter binding",
        call_parameter_order,
        [
            typed_receipt.helper_key_parameter.clone(),
            typed_receipt.helper_map_parameter.clone(),
            typed_receipt.helper_default_parameter.clone(),
        ]
    );
    reject_typed_receipt_mutation!("wrong key binding", key, typed_receipt.default.clone());
    reject_typed_receipt_mutation!("wrong default binding", default, typed_receipt.key.clone());
    reject_typed_receipt_mutation!(
        "wrong default TypeDesc",
        default_type_desc,
        typed_receipt.miss_payload_type_desc.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong Map-root TypeDesc",
        root_type_desc,
        typed_receipt.key_type_desc.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong key TypeDesc",
        key_type_desc,
        typed_receipt.miss_payload_type_desc.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong selected payload TypeDesc",
        selected_payload_type_desc,
        typed_receipt.miss_payload_type_desc.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong miss sentinel TypeDesc",
        miss_payload_type_desc,
        typed_receipt.selected_payload_type_desc.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong miss sentinel TypeDesc digest",
        miss_payload_type_desc_digest,
        "00".repeat(32)
    );
    reject_typed_receipt_mutation!(
        "wrong default pack source",
        helper_default_pack_from,
        typed_receipt.miss_payload_type_desc.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong default pack destination",
        helper_default_pack_to,
        typed_receipt.default_type_desc.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong default pack expression",
        helper_default_pack,
        typed_receipt.helper_default_load.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong helper call target",
        call_target,
        typed_receipt.owner.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong type argument",
        type_argument,
        typed_receipt.miss_payload_type_desc.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong found-arm branch binding",
        helper_found_arm_block,
        typed_receipt.helper_default_arm_block.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong default-arm branch binding",
        helper_default_arm_block,
        typed_receipt.helper_found_arm_block.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong miss-default binding",
        helper_default_load,
        typed_receipt.helper_unpack_cast.clone()
    );
    reject_typed_receipt_mutation!(
        "missing caller root Drop binding",
        caller_root_drop_point,
        NodeId("point:missing-root-drop".into())
    );
    reject_typed_receipt_mutation!(
        "wrong Set-source root local",
        caller_set_root_local,
        typed_receipt.caller_initial_root_local.clone()
    );
    reject_typed_receipt_mutation!(
        "missing Set-source root Drop binding",
        caller_set_root_drop_point,
        NodeId("point:missing-set-source-drop".into())
    );
    reject_typed_receipt_mutation!(
        "Set-source root Drop instruction bound to initial root",
        caller_set_root_drop_instruction,
        typed_receipt.caller_initial_root_drop_instruction.clone()
    );
    reject_typed_receipt_mutation!(
        "duplicate caller root Drop binding",
        caller_root_drop_point,
        typed_receipt.caller_initial_root_drop_point.clone()
    );
    reject_typed_receipt_mutation!(
        "wrong pair Drop point",
        helper_pair_drop_point,
        typed_receipt.helper_return_node.clone()
    );
    reject_typed_receipt_mutation!(
        "unpack outside the found arm",
        helper_unpack_cast,
        typed_receipt.helper_default_load.clone()
    );
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows UNIX epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mimi-get-or-default-string-cli-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create isolated get_or_default CLI directory {}: {error}",
                path.display()
            )
        });
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteIdentity {
    mir_digest: String,
    receipt_identity_digest: String,
}

fn cli(consumer: &str, explicit_mir: bool, source: &Path, output: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mimi"));
    for key in std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("MIMI_"))
        .collect::<Vec<_>>()
    {
        command.env_remove(key);
    }
    command
        .env("MIMI_ROUTE_CENSUS", "1")
        .env("MIMI_VERBOSE", "1")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg(consumer);
    if explicit_mir {
        command.arg("--mir");
    }
    command.arg(source);
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("invoke mimi {consumer}: {error}"))
}

fn output_text(output: &[u8]) -> String {
    String::from_utf8_lossy(output).into_owned()
}

fn record_fields<'a>(line: &'a str, prefix: &str, label: &str) -> BTreeMap<&'a str, &'a str> {
    let payload = line
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("{label} record is missing prefix {prefix:?}"));
    let mut fields = BTreeMap::new();
    for token in payload.split_whitespace() {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{label} has malformed receipt field {token:?}"));
        assert!(
            fields.insert(key, value).is_none(),
            "{label} repeats receipt field {key}"
        );
    }
    fields
}

fn one_record<'a>(stderr: &'a str, prefix: &str, label: &str) -> &'a str {
    let records = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .collect::<Vec<_>>();
    let [record] = records.as_slice() else {
        panic!(
            "{label} must emit exactly one record prefixed {prefix:?}; got {}:\n{stderr}",
            records.len()
        );
    };
    record
}

fn route_identity(
    output: &Output,
    consumer: &str,
    explicit_mir: bool,
    label: &str,
) -> RouteIdentity {
    assert!(
        output.status.success(),
        "{label} must succeed:\nstdout:\n{}\nstderr:\n{}",
        output_text(&output.stdout),
        output_text(&output.stderr)
    );
    let stderr = output_text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy fallback:\n{stderr}"
    );
    let records = stderr
        .lines()
        .filter(|line| line.starts_with(ROUTE_ADMISSION_PREFIX))
        .collect::<Vec<_>>();
    let [record] = records.as_slice() else {
        panic!(
            "{label} must emit exactly one canonical route-admission record; got {}:\n{stderr}",
            records.len()
        );
    };
    let fields = record_fields(record, ROUTE_ADMISSION_PREFIX, label);
    assert_eq!(fields.get("consumer").copied(), Some(consumer), "{label}");
    assert_eq!(
        fields.get("mode").copied(),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{label}"
    );
    assert_eq!(
        fields.get("checker_profile").copied(),
        Some(ROUTE_PROFILE),
        "{label} must use the independent GetOrDefaultString profile"
    );
    let mir_digest = fields
        .get("mir_digest")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its MIR digest"));
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its receipt identity digest"));
    assert_digest(mir_digest, &format!("{label} MIR digest"));
    assert_digest(
        receipt_identity_digest,
        &format!("{label} receipt identity digest"),
    );
    println!("MIMI_GET_OR_DEFAULT_STRING_ROUTE label={label} {record}");
    RouteIdentity {
        mir_digest: mir_digest.to_string(),
        receipt_identity_digest: receipt_identity_digest.to_string(),
    }
}

fn assert_digest(value: &str, label: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{label} must be a lowercase SHA-256 digest, got {value:?}"
    );
}

fn assert_native_build_receipt(
    output: &Output,
    expected: &RouteIdentity,
    explicit_mir: bool,
    label: &str,
) {
    let stderr = output_text(&output.stderr);
    let record = one_record(&stderr, NATIVE_BUILD_PREFIX, label);
    println!("MIMI_GET_OR_DEFAULT_STRING_NATIVE_RECEIPT label={label} {record}");
    let fields = record_fields(record, NATIVE_BUILD_PREFIX, label);
    let expected_consumer = if explicit_mir {
        "explicit-mir"
    } else {
        "default"
    };
    assert_eq!(
        fields.get("route_id").copied(),
        Some(ROUTE_PROFILE),
        "{label} native route id"
    );
    assert_eq!(
        fields.get("profile").copied(),
        Some(ROUTE_PROFILE),
        "{label} native profile"
    );
    assert_eq!(
        fields.get("consumer").copied(),
        Some(expected_consumer),
        "{label} native consumer mode"
    );
    assert_eq!(
        fields.get("mir_digest").copied(),
        Some(expected.mir_digest.as_str()),
        "{label} native MIR identity"
    );
    assert_eq!(
        fields.get("route_identity_digest").copied(),
        Some(expected.receipt_identity_digest.as_str()),
        "{label} native route receipt identity"
    );
}

fn assert_verifier_receipt(output: &Output, expected: &RouteIdentity, label: &str) {
    let stderr = output_text(&output.stderr);
    let record = one_record(&stderr, VERIFIER_PROVENANCE_PREFIX, label);
    println!("MIMI_GET_OR_DEFAULT_STRING_VERIFIER_RECEIPT label={label} {record}");
    let fields = record_fields(record, VERIFIER_PROVENANCE_PREFIX, label);
    assert_eq!(
        fields.get("profile").copied(),
        Some(ROUTE_PROFILE),
        "{label} verifier profile"
    );
    assert_eq!(
        fields.get("mir_digest").copied(),
        Some(expected.mir_digest.as_str()),
        "{label} verifier MIR identity"
    );
    for digest in [
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
        "source_hash",
    ] {
        let value = fields
            .get(digest)
            .unwrap_or_else(|| panic!("{label} verifier provenance omitted {digest}"));
        assert_digest(value, &format!("{label} verifier {digest}"));
    }
}

fn required_get_or_default_string_l3_valgrind() -> PathBuf {
    const WRAPPER_BODY: &[&str] = &[
        "DBGPATH=/usr/lib/debug",
        "if [ -z \"$LD_LIBRARY_PATH\" ]; then",
        "export LD_LIBRARY_PATH=\"$DBGPATH\"",
        "else",
        "export LD_LIBRARY_PATH=\"$LD_LIBRARY_PATH:$DBGPATH\"",
        "fi",
        "export GLIBCPP_FORCE_NEW=1",
        "export GLIBCXX_FORCE_NEW=1",
        "exec $0.bin \"$@\"",
    ];
    let configured = std::env::var_os("MIMI_L3_VALGRIND")
        .expect("MIMI_L3_VALGRIND must name the pinned Valgrind executable");
    let configured = PathBuf::from(configured);
    assert!(
        configured.is_absolute(),
        "MIMI_L3_VALGRIND must be absolute"
    );
    let canonical = fs::canonicalize(&configured)
        .unwrap_or_else(|error| panic!("resolve Valgrind {}: {error}", configured.display()));
    assert_eq!(
        configured, canonical,
        "MIMI_L3_VALGRIND must be a canonical path, without a symlink"
    );
    let metadata = fs::metadata(&canonical).expect("inspect Valgrind executable");
    assert!(metadata.is_file(), "Valgrind is not a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            metadata.permissions().mode() & 0o111,
            0,
            "Valgrind is not executable"
        );
    }
    let bytes = fs::read(&canonical).expect("read Valgrind executable identity");
    if bytes.starts_with(b"\x7fELF") {
        return canonical;
    }
    let wrapper = std::str::from_utf8(&bytes).expect("Valgrind wrapper is UTF-8");
    assert_eq!(
        wrapper.lines().next(),
        Some("#!/bin/sh -e"),
        "refusing an unrecognized Valgrind script shim"
    );
    let body = wrapper
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    assert_eq!(
        body.as_slice(),
        WRAPPER_BODY,
        "refusing an unrecognized Valgrind script shim"
    );
    let name = canonical
        .file_name()
        .expect("Valgrind wrapper has a filename")
        .to_string_lossy();
    let companion = canonical.with_file_name(format!("{name}.bin"));
    let companion = fs::canonicalize(&companion)
        .unwrap_or_else(|error| panic!("resolve Valgrind companion binary: {error}"));
    let companion_metadata = fs::metadata(&companion).expect("inspect Valgrind companion");
    assert!(
        companion_metadata.is_file(),
        "Valgrind companion is not a file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            companion_metadata.permissions().mode() & 0o111,
            0,
            "Valgrind companion is not executable"
        );
    }
    assert!(
        fs::read(&companion)
            .expect("read Valgrind companion")
            .starts_with(b"\x7fELF"),
        "Valgrind wrapper companion is not ELF"
    );
    canonical
}

fn write_fresh_evidence(path: &Path, bytes: &[u8]) {
    assert!(
        !path.exists() && !path.is_symlink(),
        "refusing to overwrite evidence path {}",
        path.display()
    );
    fs::write(path, bytes)
        .unwrap_or_else(|error| panic!("write evidence file {}: {error}", path.display()));
}

fn memcheck_summary_excerpt(report: &str) -> String {
    let lines = report
        .lines()
        .filter(|line| {
            [
                "definitely lost:",
                "indirectly lost:",
                "possibly lost:",
                "still reachable:",
                "ERROR SUMMARY:",
                "All heap blocks were freed",
            ]
            .iter()
            .any(|marker| line.contains(marker))
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        "No Memcheck summary lines were present in the raw report.\n".to_string()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

#[derive(Debug)]
struct GetOrDefaultStringL3RunSummary {
    fixture: &'static str,
    mode: &'static str,
    stdout_hex: String,
    exit_code: String,
    success: bool,
    strict_memcheck_passed: bool,
    identity_matches_default: bool,
}

#[test]
#[ignore = "strict get_or_default<string> native Memcheck L3; run the dedicated L3 gate script"]
fn get_or_default_string_native_default_and_explicit_mir_are_memcheck_clean() {
    let evidence_root = PathBuf::from(
        std::env::var_os("MIMI_GET_OR_DEFAULT_STRING_L3_ARTIFACT_DIR")
            .expect("L3 gate must provide an evidence directory"),
    );
    assert!(evidence_root.is_absolute() && evidence_root.is_dir());
    let native_root = evidence_root.join("native");
    fs::create_dir(&native_root)
        .unwrap_or_else(|error| panic!("create native evidence directory: {error}"));
    let expected_run_summary = format!(
        "route_profile={ROUTE_PROFILE}\nglobal_ordinary_map_owned_any_v2_production_ready=false\nfixture_count=4\nmemcheck_run_count=8\ndefault_and_explicit_mir_route_identities_match=true\nrun[0] fixture=hit mode=default stdout_hex=73746f726564206869740a exit_code=0 success=true strict_memcheck=pass identity_matches_default=true\nrun[1] fixture=hit mode=explicit-mir stdout_hex=73746f726564206869740a exit_code=0 success=true strict_memcheck=pass identity_matches_default=true\nrun[2] fixture=miss mode=default stdout_hex=64656661756c74206d6973730a exit_code=0 success=true strict_memcheck=pass identity_matches_default=true\nrun[3] fixture=miss mode=explicit-mir stdout_hex=64656661756c74206d6973730a exit_code=0 success=true strict_memcheck=pass identity_matches_default=true\nrun[4] fixture=empty-hit mode=default stdout_hex=0a exit_code=0 success=true strict_memcheck=pass identity_matches_default=true\nrun[5] fixture=empty-hit mode=explicit-mir stdout_hex=0a exit_code=0 success=true strict_memcheck=pass identity_matches_default=true\nrun[6] fixture=reachable-helper mode=default stdout_hex=68656c706572206869740a exit_code=0 success=true strict_memcheck=pass identity_matches_default=true\nrun[7] fixture=reachable-helper mode=explicit-mir stdout_hex=68656c706572206869740a exit_code=0 success=true strict_memcheck=pass identity_matches_default=true\n"
    );
    write_fresh_evidence(
        &native_root.join("expected-run-summary.txt"),
        expected_run_summary.as_bytes(),
    );
    let valgrind = required_get_or_default_string_l3_valgrind();
    write_fresh_evidence(
        &native_root.join("valgrind-tool-identity.txt"),
        format!("path={}\n", valgrind.display()).as_bytes(),
    );

    let version = Command::new(&valgrind).arg("--version").output();
    let version = match version {
        Ok(version) => {
            write_fresh_evidence(
                &native_root.join("valgrind-version.stdout"),
                &version.stdout,
            );
            write_fresh_evidence(
                &native_root.join("valgrind-version.stderr"),
                &version.stderr,
            );
            write_fresh_evidence(
                &native_root.join("valgrind-version.status"),
                format!(
                    "spawned=true\nexit_code={:?}\nsuccess={}\n",
                    version.status.code(),
                    version.status.success()
                )
                .as_bytes(),
            );
            version
        }
        Err(error) => {
            write_fresh_evidence(&native_root.join("valgrind-version.stdout"), b"");
            write_fresh_evidence(&native_root.join("valgrind-version.stderr"), b"");
            write_fresh_evidence(
                &native_root.join("valgrind-version.spawn-error.txt"),
                format!("{error}\n").as_bytes(),
            );
            write_fresh_evidence(
                &native_root.join("valgrind-version.status"),
                b"spawned=false\nexit_code=unavailable\nsuccess=false\nspawn_error=valgrind-version.spawn-error.txt\n",
            );
            panic!("query Valgrind version: {error}");
        }
    };
    assert!(version.status.success(), "Valgrind --version failed");
    assert!(
        output_text(&version.stdout).trim().starts_with("valgrind-"),
        "Valgrind version output did not identify Valgrind: {}",
        output_text(&version.stdout)
    );
    let cases = [
        (
            "hit",
            "mir_ordinary_map_owned_any_v2_get_or_default_string_hit.mimi",
            EXPECTED_HIT_STDOUT,
        ),
        (
            "miss",
            "mir_ordinary_map_owned_any_v2_get_or_default_string_miss.mimi",
            EXPECTED_MISS_STDOUT,
        ),
        (
            "empty-hit",
            "mir_ordinary_map_owned_any_v2_get_or_default_string_hit_empty.mimi",
            EXPECTED_EMPTY_HIT_STDOUT,
        ),
        (
            "reachable-helper",
            "mir_ordinary_map_owned_any_v2_get_or_default_string_reachable_helper.mimi",
            EXPECTED_HELPER_STDOUT,
        ),
    ];
    let mut run_summaries: Vec<GetOrDefaultStringL3RunSummary> =
        Vec::with_capacity(cases.len() * 2);
    for (fixture_label, fixture_name, expected_stdout) in cases {
        let source = fixture_path(fixture_name);
        let fixture_root = native_root.join(fixture_label);
        fs::create_dir(&fixture_root)
            .unwrap_or_else(|error| panic!("create {fixture_label} evidence directory: {error}"));
        write_fresh_evidence(
            &fixture_root.join("source.fixture.mimi"),
            &fs::read(&source)
                .unwrap_or_else(|error| panic!("read fixture {}: {error}", source.display())),
        );

        let mut shared_identity: Option<RouteIdentity> = None;
        let mut default_summary_index = None;
        for explicit_mir in [false, true] {
            let mode = if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            };
            let run_root = fixture_root.join(mode);
            fs::create_dir(&run_root)
                .unwrap_or_else(|error| panic!("create {fixture_label}/{mode} evidence: {error}"));
            let binary = run_root.join("program");
            let build = cli("build", explicit_mir, &source, Some(&binary));
            let label = format!("GetOrDefaultString L3 {fixture_label} {mode}");
            write_fresh_evidence(&run_root.join("build.stdout"), &build.stdout);
            write_fresh_evidence(&run_root.join("build.stderr"), &build.stderr);
            write_fresh_evidence(
                &run_root.join("build.argv.txt"),
                format!(
                    "mimi build {}{} --output {}\n",
                    if explicit_mir { "--mir " } else { "" },
                    source.display(),
                    binary.display()
                )
                .as_bytes(),
            );
            write_fresh_evidence(
                &run_root.join("build.status"),
                format!(
                    "exit_code={:?}\nsuccess={}\n",
                    build.status.code(),
                    build.status.success()
                )
                .as_bytes(),
            );
            let identity = route_identity(&build, "build", explicit_mir, &label);
            assert_native_build_receipt(&build, &identity, explicit_mir, &label);
            assert!(binary.is_file(), "{label} did not produce a native binary");
            if let Some(previous) = &shared_identity {
                assert_eq!(
                    &identity, previous,
                    "{label} changed the exact MIR or route receipt identity"
                );
            } else {
                shared_identity = Some(identity.clone());
                default_summary_index = Some(run_summaries.len());
            }

            let valgrind_log = run_root.join("valgrind.log");
            let memcheck = Command::new(&valgrind)
                .args([
                    "--leak-check=full",
                    "--show-leak-kinds=all",
                    "--errors-for-leak-kinds=all",
                    "--error-exitcode=97",
                ])
                .arg(format!("--log-file={}", valgrind_log.display()))
                .arg(&binary)
                .output()
                .unwrap_or_else(|error| panic!("run Valgrind for {label}: {error}"));
            write_fresh_evidence(&run_root.join("valgrind.stdout"), &memcheck.stdout);
            write_fresh_evidence(&run_root.join("valgrind.stderr"), &memcheck.stderr);
            write_fresh_evidence(
                &run_root.join("valgrind.status"),
                format!(
                    "exit_code={:?}\nsuccess={}\nexpected_stdout_hex={}\nobserved_stdout_hex={}\n",
                    memcheck.status.code(),
                    memcheck.status.success(),
                    expected_stdout
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                    memcheck
                        .stdout
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                )
                .as_bytes(),
            );
            let report = fs::read(&valgrind_log).unwrap_or_else(|error| {
                panic!(
                    "Valgrind did not retain its raw report for {label} at {}: {error}; stderr={}",
                    valgrind_log.display(),
                    output_text(&memcheck.stderr)
                )
            });
            let report_text = output_text(&report);
            write_fresh_evidence(
                &run_root.join("memcheck-summary.raw.txt"),
                memcheck_summary_excerpt(&report_text).as_bytes(),
            );
            let summary = strict_memcheck_summary::parse(&report_text);
            write_fresh_evidence(
                &run_root.join("memcheck-summary.txt"),
                summary.rendered.as_bytes(),
            );
            assert_eq!(
                memcheck.status.code(),
                Some(0),
                "{label} Valgrind/native exit status; stderr={} report={}",
                output_text(&memcheck.stderr),
                output_text(&report)
            );
            assert_eq!(
                memcheck.stdout.as_slice(),
                expected_stdout,
                "{label} native output"
            );

            let identity_matches_default = explicit_mir;
            if explicit_mir {
                let index = default_summary_index
                    .expect("explicit MIR run follows a validated default identity");
                run_summaries[index].identity_matches_default = identity_matches_default;
            }
            let stdout_hex = memcheck
                .stdout
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            println!(
                "MIMI_GET_OR_DEFAULT_STRING_L3 fixture={fixture_label} mode={mode} profile={ROUTE_PROFILE} mir_digest={} route_identity_digest={} stdout_hex={} memcheck_summary={}still_reachable_bytes={} still_reachable_blocks={}",
                shared_identity
                    .as_ref()
                    .expect("route identity was recorded")
                    .mir_digest,
                shared_identity
                    .as_ref()
                    .expect("route identity was recorded")
                    .receipt_identity_digest,
                stdout_hex,
                summary.rendered.replace('\n', "|"),
                summary.still_reachable_bytes,
                summary.still_reachable_blocks,
            );
            run_summaries.push(GetOrDefaultStringL3RunSummary {
                fixture: fixture_label,
                mode,
                stdout_hex,
                exit_code: memcheck
                    .status
                    .code()
                    .map_or_else(|| "signal".to_string(), |code| code.to_string()),
                success: memcheck.status.success(),
                strict_memcheck_passed: true,
                identity_matches_default,
            });
        }
    }
    let run_count = run_summaries.len();
    let identities_match = run_count == cases.len() * 2
        && run_summaries.iter().all(|run| run.identity_matches_default);
    let mut observed_run_summary = format!(
        "route_profile={ROUTE_PROFILE}\nglobal_ordinary_map_owned_any_v2_production_ready={}\nfixture_count={}\nmemcheck_run_count={run_count}\ndefault_and_explicit_mir_route_identities_match={identities_match}\n",
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
        cases.len()
    );
    for (index, run) in run_summaries.iter().enumerate() {
        use std::fmt::Write as _;
        writeln!(
            observed_run_summary,
            "run[{index}] fixture={} mode={} stdout_hex={} exit_code={} success={} strict_memcheck={} identity_matches_default={}",
            run.fixture,
            run.mode,
            run.stdout_hex,
            run.exit_code,
            run.success,
            if run.strict_memcheck_passed {
                "pass"
            } else {
                "fail"
            },
            run.identity_matches_default
        )
        .expect("append deterministic String GetOrDefault L3 run summary");
    }
    write_fresh_evidence(
        &native_root.join("run-summary.txt"),
        observed_run_summary.as_bytes(),
    );
    assert_eq!(run_count, cases.len() * 2);
    assert!(
        identities_match,
        "default and explicit MIR route identities match"
    );
}

#[test]
fn get_or_default_string_cli_default_and_explicit_mir_routes_match_for_all_consumers() {
    let temporary = TemporaryDirectory::new();
    for (fixture_name, expected_stdout) in [
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_hit.mimi",
            EXPECTED_HIT_STDOUT,
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_miss.mimi",
            EXPECTED_MISS_STDOUT,
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_hit_empty.mimi",
            EXPECTED_EMPTY_HIT_STDOUT,
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_reachable_helper.mimi",
            EXPECTED_HELPER_STDOUT,
        ),
    ] {
        let source = fixture_path(fixture_name);
        let fixture_label = if fixture_name.contains("_hit.") {
            "hit"
        } else if fixture_name.contains("_reachable_helper.") {
            "reachable-helper"
        } else if fixture_name.contains("_hit_empty.") {
            "empty-hit"
        } else {
            "miss"
        };
        let mut shared_identity: Option<RouteIdentity> = None;
        for (consumer, explicit_mir) in [
            ("run", false),
            ("run", true),
            ("build", false),
            ("build", true),
            ("verify", false),
            ("verify", true),
        ] {
            let label = format!(
                "GetOrDefaultString {} {consumer} {}",
                fixture_name,
                if explicit_mir {
                    "explicit --mir"
                } else {
                    "default"
                }
            );
            let binary = (consumer == "build").then(|| {
                temporary.path().join(format!(
                    "{fixture_label}-{consumer}-{}",
                    if explicit_mir { "mir" } else { "default" }
                ))
            });
            let output = cli(consumer, explicit_mir, &source, binary.as_deref());
            let identity = route_identity(&output, consumer, explicit_mir, &label);
            if consumer == "run" {
                assert_observed_output(
                    &format!(
                        "{fixture_label}-cli-run-{}",
                        if explicit_mir {
                            "explicit-mir"
                        } else {
                            "default"
                        }
                    ),
                    &output.stdout,
                    expected_stdout,
                );
            } else if consumer == "build" {
                assert_native_build_receipt(&output, &identity, explicit_mir, &label);
                let binary = binary.expect("build output path");
                assert!(binary.is_file(), "{label} produced no native executable");
                let native = Command::new(&binary)
                    .current_dir(env!("CARGO_MANIFEST_DIR"))
                    .output()
                    .unwrap_or_else(|error| panic!("execute {label} native binary: {error}"));
                assert_eq!(native.status.code(), Some(0), "{label} native exit code");
                assert_observed_output(
                    &format!(
                        "{fixture_label}-native-build-{}",
                        if explicit_mir {
                            "explicit-mir"
                        } else {
                            "default"
                        }
                    ),
                    &native.stdout,
                    expected_stdout,
                );
                assert!(
                    native.stderr.is_empty(),
                    "{label} native stderr: {}",
                    output_text(&native.stderr)
                );
            } else if consumer == "verify" {
                assert_verifier_receipt(&output, &identity, &label);
            }
            if let Some(previous) = &shared_identity {
                assert_eq!(
                    &identity, previous,
                    "{label} changed the MIR or route receipt identity"
                );
            } else {
                shared_identity = Some(identity);
            }
        }
    }
}

fn assert_not_get_or_default_string(source: &CheckedFixture, label: &str) {
    let admission = classify_canonical_mir_route_admission(&source.checked);
    assert_ne!(
        admission.ordinary_map_owned_any_v2,
        mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
        "{label} must not receive GetOrDefaultString Checker authority"
    );
    match materialize_canonical_mir_route(&source.checked, Some(&source.excluded_sources)) {
        Ok(route) => {
            assert_ne!(
                route.admission.ordinary_map_owned_any_v2,
                mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
                "{label} must not materialize the GetOrDefaultString MIR profile"
            );
            assert!(
                !mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultString
                    .is_materialized(&route),
                "{label} must not carry the profile's dedicated MIR receipt"
            );
            assert!(
                !route.materialized_ordinary_map_owned_any_v2_candidate,
                "{label} must leave broad V2 readiness closed"
            );
        }
        Err(mimi::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
            admission,
            ..
        }) => assert_ne!(
            admission.ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
            "{label} compatibility error must carry an outside-profile admission"
        ),
        Err(
            error @ mimi::core::mir::CanonicalMirRouteMaterializationError::Complete {
                stage: mimi::core::mir::CanonicalMirRouteFailureStage::Coverage,
                ..
            },
        ) => assert_eq!(
            error.diagnostic_code(),
            Some("MIR-COVERAGE-001"),
            "{label} complete route rejection must use the stable coverage diagnostic"
        ),
        Err(error) => panic!(
            "{label} must be outside the profile or fail closed at route coverage, not fail during MIR construction: {error}"
        ),
    }
}

fn assert_cli_not_get_or_default_profile(output: &Output, label: &str) {
    let stderr = output_text(&output.stderr);
    for record in stderr
        .lines()
        .filter(|line| line.starts_with(ROUTE_ADMISSION_PREFIX))
    {
        let fields = record_fields(record, ROUTE_ADMISSION_PREFIX, label);
        assert_ne!(
            fields.get("checker_profile").copied(),
            Some(ROUTE_PROFILE),
            "{label} must not advertise the stdlib GetOrDefaultString profile"
        );
    }
}

fn assert_cli_near_miss(
    fixture_name: &str,
    helper_is_valid_ordinary_source: bool,
    expected_codes: &[&str],
    label: &str,
) {
    let temporary = TemporaryDirectory::new();
    let source = fixture_path(fixture_name);
    for (consumer, explicit_mir) in [
        ("run", false),
        ("run", true),
        ("build", false),
        ("build", true),
        ("verify", false),
        ("verify", true),
    ] {
        let mode = format!(
            "{label} {consumer} {}",
            if explicit_mir {
                "explicit --mir"
            } else {
                "default"
            }
        );
        let binary = (consumer == "build").then(|| {
            temporary.path().join(format!(
                "near-miss-{}-{}",
                consumer,
                if explicit_mir { "mir" } else { "default" }
            ))
        });
        let output = cli(consumer, explicit_mir, &source, binary.as_deref());
        let stderr = output_text(&output.stderr);
        let diagnostic = format!("{}\n{stderr}", output_text(&output.stdout));

        if helper_is_valid_ordinary_source {
            if output.status.success() {
                if consumer == "run" {
                    assert_observed_output(
                        &format!(
                            "user-helper-cli-run-{}",
                            if explicit_mir {
                                "explicit-mir"
                            } else {
                                "default"
                            }
                        ),
                        &output.stdout,
                        b"73\n",
                    );
                } else if consumer == "build" {
                    let binary = binary.as_ref().expect("build executable path");
                    assert!(binary.is_file(), "{mode} produced no native executable");
                    let native = Command::new(binary)
                        .current_dir(env!("CARGO_MANIFEST_DIR"))
                        .output()
                        .unwrap_or_else(|error| panic!("execute {mode}: {error}"));
                    assert!(native.status.success(), "{mode} native execution failed");
                    assert_observed_output(
                        &format!(
                            "user-helper-native-build-{}",
                            if explicit_mir {
                                "explicit-mir"
                            } else {
                                "default"
                            }
                        ),
                        &native.stdout,
                        b"73\n",
                    );
                }
            } else {
                assert!(
                    expected_codes.iter().any(|code| diagnostic.contains(code))
                        || diagnostic.contains("MIR-COVERAGE-001"),
                    "{mode} must fail with an expected route/source diagnostic:\n{diagnostic}"
                );
            }
        } else {
            assert!(
                !output.status.success(),
                "{mode} unexpectedly accepted a malformed GetOrDefaultString near miss:\n{diagnostic}"
            );
            assert!(
                expected_codes.iter().any(|code| diagnostic.contains(code))
                    || diagnostic.contains("MIR-COVERAGE-001"),
                "{mode} must fail with its expected Checker or route-coverage diagnostic, not an unrelated error:\n{diagnostic}"
            );
            assert!(
                !stderr.contains(NATIVE_BUILD_PREFIX),
                "{mode} reported a native receipt after near-miss rejection:\n{stderr}"
            );
            assert!(
                !stderr.contains(VERIFIER_PROVENANCE_PREFIX),
                "{mode} reported verifier provenance after near-miss rejection:\n{stderr}"
            );
        }

        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "{mode} silently retried through a legacy route:\n{stderr}"
        );
        assert_cli_not_get_or_default_profile(&output, &mode);
        if !helper_is_valid_ordinary_source {
            assert!(
                !stderr.contains("canonical route disposition: canonical"),
                "{mode} claimed an executable canonical route for a rejected near miss:\n{stderr}"
            );
            if let Some(binary) = binary {
                assert!(
                    !binary.exists(),
                    "{mode} left an executable after rejecting the near miss"
                );
            }
        }
    }
}

#[test]
fn get_or_default_string_near_miss_fixtures_stay_outside_the_profile() {
    const CHECKER_VALID_NEAR_MISSES: [&str; 2] = [
        "mir_ordinary_map_owned_any_v2_get_or_default_string_mutable_default.mimi",
        "mir_ordinary_map_owned_any_v2_get_or_default_string_mutable_payload.mimi",
    ];
    let cases: &[(&str, &[&str])] = &[
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_wrong_i32_payload.mimi",
            &["E0200", "E0242"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_wrong_bool_payload.mimi",
            &["E0200", "E0242"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_dynamic_key.mimi",
            &["E0200"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_non_string_default.mimi",
            &["E0200", "E0211", "E0242"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_non_string_instance.mimi",
            &["E0200", "E0211"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_multiple_callsites.mimi",
            &["E0200"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_missing_pair_drop.mimi",
            &["E0200"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_duplicate_pair_drop.mimi",
            &["E0304", "E0200"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_early_pair_drop.mimi",
            &["E0304", "E0200"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_missing_root_drop.mimi",
            &["E0256", "E0200"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_duplicate_root_drop.mimi",
            &["E0304", "E0200"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_missing_result_drop.mimi",
            &["E0200", "E0256"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_duplicate_result_drop.mimi",
            &["E0200", "E0304"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_extra_mutation.mimi",
            &["E0200"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_mutable_default.mimi",
            &["MIR-COVERAGE-001"],
        ),
        (
            "mir_ordinary_map_owned_any_v2_get_or_default_string_mutable_payload.mimi",
            &["MIR-COVERAGE-001"],
        ),
    ];

    for (name, expected_diagnostic_codes) in cases {
        match checked_fixture(name) {
            Ok(checked) => {
                assert_not_get_or_default_string(&checked, name);
            }
            Err(diagnostics) => {
                assert!(
                    !CHECKER_VALID_NEAR_MISSES.contains(name),
                    "{name} must pass the Checker as ordinary Mimi semantics: {diagnostics:?}"
                );
                assert!(
                    diagnostics.iter().any(|diagnostic| {
                        diagnostic
                            .code
                            .as_deref()
                            .is_some_and(|code| expected_diagnostic_codes.contains(&code))
                    }),
                    "{name} must fail with one of {expected_diagnostic_codes:?}, not an unrelated source error: {diagnostics:?}"
                );
            }
        }
        assert_cli_near_miss(name, false, expected_diagnostic_codes, name);
    }

    for name in CHECKER_VALID_NEAR_MISSES {
        let checked = checked_fixture(name).unwrap_or_else(|diagnostics| {
            panic!("{name} remains Checker-valid ordinary Mimi: {diagnostics:?}")
        });
        assert_ne!(
            classify_canonical_mir_route_admission(&checked.checked).ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
            "{name} does not qualify for the immutable-source String profile"
        );
    }
}

#[test]
fn get_or_default_string_actor_method_caller_is_rejected_before_profile_admission() {
    const FIXTURE: &str = "mir_ordinary_map_owned_any_v2_get_or_default_string_actor_caller.mimi";
    let fixture = checked_fixture(FIXTURE).expect("Actor caller fixture passes the Checker");
    let actor_method_owners = fixture
        .checked
        .actor_descriptors()
        .values()
        .flat_map(|actor| actor.methods.iter().map(|method| method.owner.clone()))
        .collect::<HashSet<_>>();
    assert!(
        !actor_method_owners.is_empty(),
        "fixture must produce Checker-owned Actor method identities"
    );

    let admission = classify_canonical_mir_route_admission(&fixture.checked);
    assert_ne!(
        admission.ordinary_map_owned_any_v2,
        mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
        "Actor-tagged caller cannot enter the profile whose early status return bypasses local Drops"
    );
    assert_not_get_or_default_string(&fixture, "Actor-tagged caller");

    let source = fixture_path(FIXTURE);
    let temporary = TemporaryDirectory::new();
    for (consumer, explicit_mir) in [
        ("run", false),
        ("run", true),
        ("build", false),
        ("build", true),
        ("verify", false),
        ("verify", true),
    ] {
        let label = format!(
            "Actor-caller {consumer} {}",
            if explicit_mir {
                "explicit --mir"
            } else {
                "default"
            }
        );
        let binary = (consumer == "build").then(|| {
            temporary.path().join(format!(
                "actor-caller-{consumer}-{}",
                if explicit_mir { "mir" } else { "default" }
            ))
        });
        let output = cli(consumer, explicit_mir, &source, binary.as_deref());
        let stderr = output_text(&output.stderr);
        assert!(
            !output.status.success(),
            "{label} unexpectedly accepted the Actor-tagged Map/Any caller:\nstdout:\n{}\nstderr:\n{stderr}",
            output_text(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "{label} emitted stdout before the route rejection:\n{}",
            output_text(&output.stdout)
        );
        assert!(
            stderr.contains("MIR-COVERAGE-001"),
            "{label} must fail at the stable canonical route coverage gate:\n{stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains("canonical route disposition: compatibility"),
            "{label} retried through a Legacy/compatibility route:\n{stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: canonical")
                && !stderr.contains("canonical route disposition: explicit canonical")
                && !stderr.contains(ROUTE_ADMISSION_PREFIX),
            "{label} emitted a canonical route admission after rejection:\n{stderr}"
        );
        assert!(
            !stderr.contains(NATIVE_BUILD_PREFIX) && !stderr.contains(VERIFIER_PROVENANCE_PREFIX),
            "{label} emitted a native/verifier receipt after rejection:\n{stderr}"
        );
        assert_cli_not_get_or_default_profile(&output, &label);
        if let Some(binary) = binary {
            assert!(
                !binary.exists(),
                "{label} left a native build artifact after rejection: {}",
                binary.display()
            );
        }
    }
}

#[test]
fn same_named_user_get_or_default_string_helper_does_not_receive_stdlib_profile() {
    const FIXTURE: &str = "mir_ordinary_map_owned_any_v2_get_or_default_string_user_helper.mimi";
    let checked = checked_fixture(FIXTURE).expect("same-name helper is valid ordinary Mimi");
    assert_not_get_or_default_string(&checked, "same-name user helper");
    assert_cli_near_miss(
        FIXTURE,
        false,
        &["MIR-COVERAGE-001"],
        "same-name user helper",
    );
}

#[test]
fn get_or_default_string_unreachable_caller_is_rejected_before_profile_admission() {
    const FIXTURE: &str =
        "mir_ordinary_map_owned_any_v2_get_or_default_string_unreachable_caller.mimi";
    let diagnostics = match checked_fixture(FIXTURE) {
        Err(diagnostics) => diagnostics,
        Ok(_) => panic!("an orphan cold() callsite cannot receive a main-rooted profile receipt"),
    };
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("E0218")
                && diagnostic
                    .message
                    .to_ascii_lowercase()
                    .contains("get_or_default<string>")
        }),
        "the unreachable callsite fails at the dedicated Checker receipt gate: {diagnostics:?}"
    );
    assert_cli_near_miss(FIXTURE, false, &["E0218"], "unreachable caller");
}

#[test]
fn get_or_default_string_actor_tagged_shared_helper_is_rejected_without_legacy_fallback() {
    const FIXTURE: &str =
        "mir_ordinary_map_owned_any_v2_get_or_default_string_actor_shared_helper.mimi";
    assert_fixture_sha256(FIXTURE, ACTOR_SHARED_HELPER_FIXTURE_SHA256);
    let fixture = checked_fixture(FIXTURE)
        .expect("Actor method and main can both statically reach the ordinary helper in Checker");
    let admission = classify_canonical_mir_route_admission(&fixture.checked);
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::GetOrDefaultString,
        "the ordinary main-reachable helper is Checker-admitted before MIR Actor gating"
    );
    let materialization =
        materialize_canonical_mir_route(&fixture.checked, Some(&fixture.excluded_sources));
    let error = materialization.expect_err(
        "the Actor method's direct helper edge must put the profile caller behind the MIR Actor status gate",
    );
    assert!(
        format!("{error:?}").to_ascii_lowercase().contains("actor"),
        "MIR materialization rejects through its Actor execution/status graph gate: {error}"
    );

    let source = fixture_path(FIXTURE);
    let temporary = TemporaryDirectory::new();
    for (consumer, explicit_mir) in [
        ("run", false),
        ("run", true),
        ("build", false),
        ("build", true),
        ("verify", false),
        ("verify", true),
    ] {
        let output_path = (consumer == "build").then(|| {
            temporary.path().join(format!(
                "actor-shared-helper-{consumer}-{}",
                if explicit_mir { "mir" } else { "default" }
            ))
        });
        let output = cli(consumer, explicit_mir, &source, output_path.as_deref());
        let label = format!(
            "Actor-shared-helper {consumer} {}",
            if explicit_mir {
                "explicit --mir"
            } else {
                "default"
            }
        );
        let stderr = output_text(&output.stderr);
        let diagnostic = format!("{}\n{stderr}", output_text(&output.stdout));
        assert!(
            !output.status.success(),
            "{label} unexpectedly executed a Checker-admitted helper through an untagged route:\n{diagnostic}"
        );
        assert!(
            output.stdout.is_empty(),
            "{label} emitted stdout before the route rejection:\n{}",
            output_text(&output.stdout)
        );
        assert!(
            stderr.contains("MIR-COVERAGE-001"),
            "{label} must fail at the stable canonical route coverage gate:\n{stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains("canonical route disposition: compatibility"),
            "{label} retried through a Legacy/compatibility route after Actor status rejection:\n{stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: canonical")
                && !stderr.contains("canonical route disposition: explicit canonical")
                && !stderr.contains("canonical route disposition: verifier-only")
                && !stderr.contains(ROUTE_ADMISSION_PREFIX),
            "{label} emitted a successful route admission after rejection:\n{stderr}"
        );
        assert!(
            !stderr.contains(NATIVE_BUILD_PREFIX) && !stderr.contains(VERIFIER_PROVENANCE_PREFIX),
            "{label} emitted a native/verifier receipt after Actor rejection:\n{stderr}"
        );
        if let Some(output_path) = output_path {
            assert!(
                !output_path.exists(),
                "{label} left a native build artifact after rejection: {}",
                output_path.display()
            );
        }
    }
}

#[test]
fn get_or_default_string_cold_nominal_set_is_rejected_globally() {
    let source_text = r#"
use std::maps;
type Label { text: string }

func main() -> i32 {
    let payload = "stored hit"
    let fallback = "unused hit default"
    let empty = map_new()
    let root = map_set(empty, "hits", payload)
    let observed = get_or_default::<string>(root, "hits", fallback)
    println(observed)
    drop(observed)
    drop(empty)
    drop(root)
    drop(payload)
    0
}

func cold_label_map_set() -> i32 {
    let label = Label { text: "cold nominal payload" }
    let empty = map_new()
    let root = map_set(empty, "cold-label", label)
    drop(empty)
    drop(root)
    drop(label)
    0
}
"#;
    let fixture = checked_inline_source(source_text)
        .expect("cold nominal Set coexists with the String caller in a Checker-valid program");
    let admission = classify_canonical_mir_route_admission(&fixture.checked);
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage,
        "the cold Label Set raw V2 signal must not disappear outside the String executable scope"
    );
    let materialization =
        materialize_canonical_mir_route(&fixture.checked, Some(&fixture.excluded_sources));
    assert!(
        materialization.is_err(),
        "cold nominal V2 signal prevents String MIR admission"
    );

    let temporary = TemporaryDirectory::new();
    let source_path = temporary.path().join("cold-label-set.mimi");
    fs::write(&source_path, source_text).expect("write isolated cold nominal source");
    for (consumer, explicit_mir) in [
        ("run", false),
        ("run", true),
        ("build", false),
        ("build", true),
        ("verify", false),
        ("verify", true),
    ] {
        let label = format!(
            "cold nominal Set {consumer} {}",
            if explicit_mir {
                "explicit --mir"
            } else {
                "default"
            }
        );
        let output_path = (consumer == "build").then(|| {
            temporary.path().join(format!(
                "cold-label-set-{consumer}-{}",
                if explicit_mir { "mir" } else { "default" }
            ))
        });
        let output = cli(consumer, explicit_mir, &source_path, output_path.as_deref());
        let stderr = output_text(&output.stderr);
        let diagnostic = format!("{}\n{stderr}", output_text(&output.stdout));
        assert!(
            !output.status.success(),
            "{label} accepted the cold nominal Set or fell through to a legacy consumer:\n{diagnostic}"
        );
        assert!(
            diagnostic.contains("MIR-COVERAGE-001")
                || diagnostic
                    .to_ascii_lowercase()
                    .contains("incompletecoverage"),
            "{label} must fail at route coverage for the unmatched cold V2 signal:\n{diagnostic}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "{label} silently retried through legacy after global V2 census failure:\n{stderr}"
        );
        assert!(
            !stderr.contains(NATIVE_BUILD_PREFIX) && !stderr.contains(VERIFIER_PROVENANCE_PREFIX),
            "{label} emitted backend evidence after global V2 census rejection:\n{stderr}"
        );
        assert_cli_not_get_or_default_profile(&output, &label);
        if let Some(output_path) = output_path {
            assert!(
                !output_path.exists(),
                "{label} left an executable after failing admission"
            );
        }
    }
}
