//! Checker-bound canonical MIR projection for literal FFI declaration roots.
//!
//! This profile exists for the narrow case where every called FFI declaration
//! contract is a direct boolean literal and every active FFI call has literal
//! arguments. It does not authorize execution: the MIR graph carries only the
//! checked `Requires`/`Ensures` predicates, while this private receipt binds
//! them to the complete Checker declaration, callsite, and source-root census.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    lower::ResolvedDeclarationExpressionKind, PrimitiveType, ResolvedCallee,
    ResolvedFfiContractRoot, ResolvedLiteral, ResolvedType, ResolvedTypeTable,
};
use crate::core::mir::{
    CanonicalMirRouteReceipt, MirBlock, MirBlockId, MirContract, MirContractExpr, MirContractKind,
    MirFunction, MirOwnershipSummary, MirTerminator,
};
use crate::core::{CheckedProgram, NodeId};
use crate::span::Span;

pub(crate) const MIR_FFI_LITERAL_CONTRACT_PROFILE_V1: &str = "verifier-ffi-literal-contract-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MirFfiLiteralBoolRootV1 {
    pub(crate) node: NodeId,
    pub(crate) value: bool,
    typed_root_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MirFfiLiteralCallsiteV1 {
    pub(crate) node: NodeId,
    pub(crate) owner: NodeId,
    pub(crate) span: Span,
    declaration: NodeId,
    typed_call_fingerprint: String,
}

/// One declaration-level result in the literal FFI profile. The first
/// Checker callsite is retained for the historical diagnostic span; every
/// exact callsite and owner remains sealed in the profile receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MirFfiLiteralDeclarationV1 {
    pub(crate) declaration: NodeId,
    pub(crate) symbol: String,
    pub(crate) requires: Option<MirFfiLiteralBoolRootV1>,
    pub(crate) ensures: Option<MirFfiLiteralBoolRootV1>,
    pub(crate) mir_owner: NodeId,
    pub(crate) callsites: Vec<MirFfiLiteralCallsiteV1>,
}

#[derive(Debug, Clone)]
pub(crate) struct MirFfiLiteralContractProfileV1 {
    program: super::MirProgram,
    route_receipt: CanonicalMirRouteReceipt,
    declarations: Vec<MirFfiLiteralDeclarationV1>,
    source_census_digest: String,
}

impl MirFfiLiteralContractProfileV1 {
    pub(crate) fn program(&self) -> &super::MirProgram {
        &self.program
    }

    pub(crate) fn route_receipt(&self) -> &CanonicalMirRouteReceipt {
        &self.route_receipt
    }

    pub(crate) fn declarations(&self) -> &[MirFfiLiteralDeclarationV1] {
        &self.declarations
    }

    /// Validate the sealed VerifierOnly graph and prove that every returned
    /// Checker root is represented once by a canonical MIR boolean contract.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.program.purpose() != super::MirProgramPurpose::VerifierOnly {
            return Err("literal FFI MIR profile is not sealed VerifierOnly".into());
        }
        if self.route_receipt.profile != MIR_FFI_LITERAL_CONTRACT_PROFILE_V1 {
            return Err("literal FFI MIR profile carries an unexpected route receipt".into());
        }
        self.route_receipt.validate_against_program(&self.program)?;
        if self.declarations.is_empty() {
            return Err("literal FFI MIR profile has no called declaration contracts".into());
        }
        let mut expected_owners = BTreeSet::new();
        let mut seen_callsites = BTreeSet::new();
        for declaration in &self.declarations {
            if declaration.callsites.is_empty() {
                return Err(format!(
                    "literal FFI declaration '{}' has no Checker body callsite",
                    declaration.symbol
                ));
            }
            if !expected_owners.insert(declaration.mir_owner.clone()) {
                return Err("literal FFI MIR profile repeats a synthetic owner".into());
            }
            if declaration.callsites.iter().any(|site| {
                site.declaration != declaration.declaration
                    || site.typed_call_fingerprint.is_empty()
                    || site.owner.0.trim().is_empty()
                    || !seen_callsites.insert(site.node.clone())
            }) {
                return Err(format!(
                    "literal FFI declaration '{}' contains a callsite for another declaration",
                    declaration.symbol
                ));
            }
            let expected_contracts = declaration
                .requires
                .iter()
                .map(|root| {
                    if root.typed_root_fingerprint.is_empty() {
                        return Err(format!(
                            "literal FFI Requires root '{}' has no Checker fingerprint",
                            root.node.0
                        ));
                    }
                    Ok((root.node.clone(), MirContractKind::Requires, root.value))
                })
                .chain(declaration.ensures.iter().map(|root| {
                    if root.typed_root_fingerprint.is_empty() {
                        return Err(format!(
                            "literal FFI Ensures root '{}' has no Checker fingerprint",
                            root.node.0
                        ));
                    }
                    Ok((root.node.clone(), MirContractKind::Ensures, root.value))
                }))
                .collect::<Result<Vec<_>, String>>()?;
            let Some(function) = self.program.functions().get(&declaration.mir_owner) else {
                return Err(format!(
                    "literal FFI MIR function for '{}' is missing",
                    declaration.symbol
                ));
            };
            let observed_contracts = function
                .contracts
                .iter()
                .map(|contract| {
                    let MirContractExpr::Bool(value) = &contract.condition else {
                        return Err(format!(
                            "literal FFI MIR contract '{}' is not a boolean constant",
                            contract.id.0
                        ));
                    };
                    Ok((contract.id.clone(), contract.kind, *value))
                })
                .collect::<Result<Vec<_>, String>>()?;
            if observed_contracts != expected_contracts
                || function.blocks.len() != 1
                || function.values.len() != 0
                || function.proof_worklist.is_some()
                || function.blocks.values().any(|block| {
                    !block.instructions.is_empty()
                        || !matches!(&block.terminator, MirTerminator::Return { value: None })
                })
            {
                return Err(format!(
                    "literal FFI MIR function for '{}' contains facts outside its checked boolean roots",
                    declaration.symbol
                ));
            }
        }
        if self.program.functions().len() != expected_owners.len()
            || self
                .program
                .functions()
                .keys()
                .any(|owner| !expected_owners.contains(owner))
        {
            return Err("literal FFI MIR graph contains an unreceipted owner".into());
        }
        if self.source_census_digest.len() != 64
            || !self
                .source_census_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("literal FFI source census digest is malformed".into());
        }
        let expected_source_owner_prefix =
            format!("verifier-only-ffi-literal:{}/", self.source_census_digest);
        if !self
            .program
            .functions()
            .keys()
            .all(|owner| owner.0.starts_with(&expected_source_owner_prefix))
        {
            return Err("literal FFI MIR graph lost its Checker census commitment".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct LiteralRoot {
    node: NodeId,
    value: bool,
    fingerprint: String,
}

fn bool_literal_root(
    program: &CheckedProgram,
    declaration: &crate::core::ir::ResolvedFfiDeclarationContract,
    root: &ResolvedFfiContractRoot,
    kind: ResolvedDeclarationExpressionKind,
) -> Result<Option<LiteralRoot>, ()> {
    let node = match root {
        ResolvedFfiContractRoot::Absent => return Ok(None),
        ResolvedFfiContractRoot::Ready { node_id } => node_id,
        ResolvedFfiContractRoot::Deferred { .. } => return Err(()),
    };
    let typed_root = program
        .declaration_expression_bodies()
        .get(node)
        .ok_or(())?;
    let body = typed_root.body.as_ref().map_err(|_| ())?;
    if typed_root.node_id != *node
        || typed_root.owner != declaration.declaration
        || typed_root.kind != kind
        || body.owner != declaration.declaration
        || !body.root.statements.is_empty()
    {
        return Err(());
    }
    let expression = body.root.result.as_deref().ok_or(())?;
    if expression.node_id != *node {
        return Err(());
    }
    match &expression.kind {
        crate::core::ir::ResolvedExprKind::Literal(ResolvedLiteral::Bool(value)) => {
            Ok(Some(LiteralRoot {
                node: node.clone(),
                value: *value,
                fingerprint: format!("{typed_root:#?}"),
            }))
        }
        _ => Err(()),
    }
}

fn literalish(expression: &crate::core::ir::ResolvedExpr) -> bool {
    match &expression.kind {
        crate::core::ir::ResolvedExprKind::Literal(_) => true,
        crate::core::ir::ResolvedExprKind::Cast { value, .. } => literalish(value),
        _ => false,
    }
}

fn checked_call_expression(
    program: &CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> Result<Option<crate::core::ir::ResolvedExpr>, String> {
    let expected_callee = match site.resolved_callee.as_ref() {
        Some(callee) => callee,
        None => return Ok(None),
    };
    let is_expected_call = |expression: &crate::core::ir::ResolvedExpr| {
        matches!(
            &expression.kind,
            crate::core::ir::ResolvedExprKind::Call(call)
                if matches!(&call.callee, ResolvedCallee::Extern(_))
                    && &call.callee == expected_callee
        )
    };
    for (owner, body) in program.resolved_bodies() {
        let expressions = body
            .call_expressions(program.resolved_types(), program.resolved_type_targets())
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        if let Some(expression) = expressions.get(&site.node_id) {
            if is_expected_call(expression) {
                if owner.0 != site.owner || body.owner.0 != site.owner {
                    return Ok(None);
                }
                return Ok(Some(expression.clone()));
            }
        }
    }
    for typed_root in program.declaration_expression_bodies().values() {
        let Ok(body) = &typed_root.body else {
            continue;
        };
        let expressions = body
            .call_expressions(program.resolved_types(), program.resolved_type_targets())
            .map_err(|errors| format!("TOOL-RESOLUTION-001: {errors:?}"))?;
        if let Some(expression) = expressions.get(&site.node_id) {
            if is_expected_call(expression) {
                return Ok(Some(expression.clone()));
            }
        }
    }
    Ok(None)
}

fn source_census_digest(
    program: &CheckedProgram,
    typed_calls: &[(NodeId, String)],
    roots: &[(NodeId, String)],
) -> String {
    let declarations = program
        .ffi_declarations()
        .values()
        .map(|declaration| format!("{declaration:#?}"))
        .collect::<Vec<_>>();
    let calls = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| site.kind == crate::core::ResolvedCallKind::Extern)
        .map(|site| format!("{site:#?}"))
        .collect::<Vec<_>>();
    let root_census = program
        .checked_ffi_source_roots()
        .iter()
        .map(|root| format!("{root:#?}"))
        .collect::<Vec<_>>();
    let material = format!(
        "mimi-mir-ffi-literal-source-census-v1\ndeclarations={declarations:#?}\ncalls={calls:#?}\nchecker_roots={root_census:#?}\ntyped_calls={typed_calls:#?}\ntyped_contract_roots={roots:#?}"
    );
    blake3::hash(material.as_bytes()).to_hex().to_string()
}

/// Construct an explicit canonical MIR VerifierOnly profile for a complete
/// Checker literal-contract census. `Ok(None)` is an admission miss; malformed
/// checker identity is returned as a resolution error instead of being hidden
/// by the compatibility selector.
pub(crate) fn from_checked_program_ffi_literal_contracts(
    program: &CheckedProgram,
) -> Result<Option<MirFfiLiteralContractProfileV1>, String> {
    let admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    if crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_admitted(admission) {
        return Ok(None);
    }

    let mut declared_symbols = BTreeSet::new();
    for declaration in program.ffi_declarations().values() {
        if !declared_symbols.insert(declaration.symbol.clone()) {
            return Err(format!(
                "TOOL-RESOLUTION-001: duplicate resolved extern symbol '{}'",
                declaration.symbol
            ));
        }
    }

    let root_call_nodes = program
        .checked_ffi_source_roots()
        .iter()
        .flat_map(|root| root.calls().iter().map(|call| call.call_node().clone()))
        .collect::<BTreeSet<_>>();
    let mut entries = Vec::<MirFfiLiteralDeclarationV1>::new();
    let mut entry_by_symbol = BTreeMap::<String, usize>::new();
    let mut typed_calls = Vec::new();
    let mut typed_roots = Vec::new();

    for site in program.call_sites_sorted() {
        if site.kind != crate::core::ResolvedCallKind::Extern
            || root_call_nodes.contains(&site.node_id)
        {
            continue;
        }
        let caller_owner = NodeId(site.owner.clone());
        if program
            .callable(&caller_owner)
            .is_some_and(crate::core::ResolvedCallable::has_proof_obligations)
        {
            return Ok(None);
        }
        let Some(ResolvedCallee::Extern(declaration_id)) = site.resolved_callee.as_ref() else {
            return Ok(None);
        };
        let Some(expression) = checked_call_expression(program, site)? else {
            return Ok(None);
        };
        let crate::core::ir::ResolvedExprKind::Call(call) = &expression.kind else {
            return Ok(None);
        };
        if !call
            .arguments
            .iter()
            .all(|argument| literalish(&argument.value))
        {
            return Ok(None);
        }
        typed_calls.push((site.node_id.clone(), format!("{expression:#?}")));

        let Some(declaration) = program.ffi_declarations().get(declaration_id) else {
            return Err(format!(
                "TOOL-RESOLUTION-001: missing checker-owned extern declaration for call '{}'",
                site.callee
            ));
        };
        if declaration.symbol != site.callee || declaration.validate(program).is_err() {
            return Ok(None);
        }
        if !declaration.has_declared_contract() {
            continue;
        }

        let entry_index = if let Some(index) = entry_by_symbol.get(&declaration.symbol) {
            *index
        } else {
            let requires = match bool_literal_root(
                program,
                declaration,
                &declaration.requires,
                ResolvedDeclarationExpressionKind::ExternRequires,
            ) {
                Ok(value) => value,
                Err(()) => return Ok(None),
            };
            let ensures = match bool_literal_root(
                program,
                declaration,
                &declaration.ensures,
                ResolvedDeclarationExpressionKind::ExternEnsures,
            ) {
                Ok(value) => value,
                Err(()) => return Ok(None),
            };
            if (declaration.requires.is_declared() && requires.is_none())
                || (declaration.ensures.is_declared() && ensures.is_none())
            {
                return Ok(None);
            }
            if let Some(root) = &requires {
                typed_roots.push((root.node.clone(), root.fingerprint.clone()));
            }
            if let Some(root) = &ensures {
                typed_roots.push((root.node.clone(), root.fingerprint.clone()));
            }
            let index = entries.len();
            entry_by_symbol.insert(declaration.symbol.clone(), index);
            entries.push(MirFfiLiteralDeclarationV1 {
                declaration: declaration.declaration.clone(),
                symbol: declaration.symbol.clone(),
                requires: requires.map(|root| MirFfiLiteralBoolRootV1 {
                    node: root.node,
                    value: root.value,
                    typed_root_fingerprint: root.fingerprint,
                }),
                ensures: ensures.map(|root| MirFfiLiteralBoolRootV1 {
                    node: root.node,
                    value: root.value,
                    typed_root_fingerprint: root.fingerprint,
                }),
                mir_owner: NodeId(String::new()),
                callsites: Vec::new(),
            });
            index
        };
        entries[entry_index]
            .callsites
            .push(MirFfiLiteralCallsiteV1 {
                node: site.node_id.clone(),
                owner: caller_owner,
                span: site.origin.user_span(),
                declaration: declaration.declaration.clone(),
                typed_call_fingerprint: format!("{expression:#?}"),
            });
    }

    if entries.is_empty() {
        return Ok(None);
    }
    let census_digest = source_census_digest(program, &typed_calls, &typed_roots);
    for entry in &mut entries {
        entry.mir_owner = NodeId(format!(
            "verifier-only-ffi-literal:{census_digest}/{}",
            entry.declaration.0
        ));
    }

    let mut resolved_types = ResolvedTypeTable::new();
    let bool_type = resolved_types
        .intern_resolved(ResolvedType::Primitive(PrimitiveType::Bool))
        .map_err(|error| format!("MIR-COVERAGE-001: literal FFI bool TypeDesc: {error}"))?;
    let unit_type = resolved_types
        .intern_resolved(ResolvedType::Primitive(PrimitiveType::Unit))
        .map_err(|error| format!("MIR-COVERAGE-001: literal FFI unit TypeDesc: {error}"))?;
    let catalog = crate::core::mir::types::MirTypeCatalog::from_resolved_types(&resolved_types)
        .map_err(|errors| format!("MIR-COVERAGE-001: literal FFI TypeDesc: {errors:?}"))?;

    let mut functions = BTreeMap::new();
    for declaration in &entries {
        let contracts = declaration
            .requires
            .iter()
            .map(|root| MirContract {
                id: root.node.clone(),
                kind: MirContractKind::Requires,
                condition: MirContractExpr::Bool(root.value),
                expression_types: BTreeMap::from([(Vec::new(), bool_type.clone())]),
                ffi_helper_call: None,
            })
            .chain(declaration.ensures.iter().map(|root| MirContract {
                id: root.node.clone(),
                kind: MirContractKind::Ensures,
                condition: MirContractExpr::Bool(root.value),
                expression_types: BTreeMap::from([(Vec::new(), bool_type.clone())]),
                ffi_helper_call: None,
            }))
            .collect::<Vec<_>>();
        let entry = MirBlockId::new(format!("bb:ffi-literal:{}", declaration.declaration.0))
            .map_err(|error| format!("MIR-COVERAGE-001: {error}"))?;
        let function = MirFunction {
            owner: declaration.mir_owner.clone(),
            parameters: Vec::new(),
            parameter_permissions: Some(Vec::new()),
            result: unit_type.clone(),
            entry: entry.clone(),
            values: BTreeMap::new(),
            blocks: BTreeMap::from([(
                entry.clone(),
                MirBlock {
                    id: entry,
                    parameters: Vec::new(),
                    instructions: Vec::new(),
                    terminator: MirTerminator::Return { value: None },
                },
            )]),
            contracts,
            proof_worklist: None,
            ownership: MirOwnershipSummary::default(),
        };
        functions.insert(declaration.mir_owner.clone(), function);
    }
    let mut mir = super::MirProgram::with_type_catalog(functions, catalog).map_err(|errors| {
        format!("MIR-COVERAGE-001: literal FFI MIR construction failed: {errors:?}")
    })?;
    mir.purpose = super::MirProgramPurpose::VerifierOnly;
    let route_receipt = mir.route_receipt(MIR_FFI_LITERAL_CONTRACT_PROFILE_V1);
    route_receipt
        .validate_against_program(&mir)
        .map_err(|error| format!("MIR-COVERAGE-001: literal FFI MIR receipt: {error}"))?;
    let profile = MirFfiLiteralContractProfileV1 {
        program: mir,
        route_receipt,
        declarations: entries,
        source_census_digest: census_digest,
    };
    profile
        .validate()
        .map_err(|error| format!("MIR-COVERAGE-001: literal FFI profile validation: {error}"))?;
    Ok(Some(profile))
}

/// Preserve the declaration-level legacy diagnostic anchor while retaining
/// the exact owner, node, and span for every call in the canonical receipt.
pub(crate) fn first_callsite(declaration: &MirFfiLiteralDeclarationV1) -> Option<(NodeId, Span)> {
    declaration
        .callsites
        .first()
        .map(|site| (site.owner.clone(), site.span))
}
