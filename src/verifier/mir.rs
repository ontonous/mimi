//! Z3 projection for the first Canonical MIR contract slice.
//!
//! This module intentionally has no access to `File`, `Expr`, `ResolvedBody`,
//! or source names.  It consumes only validated `MirProgram` values, the
//! canonical TypeDesc catalog, and the MIR contract predicate attached to each
//! function.  Unsupported CFG/effect/aggregate shapes become an explicit
//! `NotInTrustedSubset` result; they never fall through to the legacy verifier.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirAbiClass, MirBuiltinKind, MirLayout, MirOwnership};
use crate::core::mir::{
    MirAggregateKind, MirContractBinaryOp, MirContractExpr, MirContractKind, MirContractUnaryOp,
    MirFunction, MirInstructionKind, MirProjection, MirSwitchCase, MirTerminator, MirValueId,
};
use crate::verifier::ctx::{
    ProofArtifact, SolverSession, TrustedSubsetDomain, VerifStatus, VerificationResult,
};
use z3::ast::{Bool, Int};
use z3::SatResult;

#[derive(Debug, Clone)]
enum SymbolicValue {
    Int(Int),
    Bool(Bool),
    Tuple(Vec<SymbolicValue>),
    Record {
        nominal: crate::core::ir::NominalTypeId,
        fields: BTreeMap<crate::core::NodeId, SymbolicValue>,
    },
    /// A symbolic built-in Option/Result value.  The tag is constrained to
    /// the canonical TypeDesc discriminants when the value is introduced;
    /// payloads are keyed by stable field identity so switch bindings never
    /// infer a payload slot from source-pattern position.
    Variant {
        nominal: crate::core::ir::NominalTypeId,
        tag: Int,
        payload: BTreeMap<crate::core::NodeId, SymbolicValue>,
    },
}

#[derive(Debug, Clone)]
struct SymbolicTrap {
    condition: Vec<Bool>,
    code: String,
}

#[derive(Debug, Clone)]
struct SymbolicState {
    values: BTreeMap<MirValueId, SymbolicValue>,
    constraints: Vec<Bool>,
    traps: Vec<SymbolicTrap>,
}

#[derive(Debug, Clone)]
struct ReturnPath {
    constraints: Vec<Bool>,
    values: BTreeMap<MirValueId, SymbolicValue>,
    value: SymbolicValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarKind {
    Int { bits: u16 },
    Bool,
}

/// Verify all contract-bearing functions in a validated canonical MIR
/// program.  The caller owns source loading and MIR construction; this entry
/// point deliberately accepts no frontend artifact and cannot invoke a
/// fallback verifier.
pub(crate) fn verify_program(
    program: &MirProgram,
    source_hash: String,
) -> Result<Vec<VerificationResult>, String> {
    let mut session = SolverSession::new(super::ctx::DEFAULT_TIMEOUT_MS)?;
    let mir_hash = canonical_mir_hash(program);
    let mut results = Vec::new();

    for function in program.functions().values() {
        if function.contracts.is_empty() {
            continue;
        }
        session.reset();
        let started = Instant::now();
        let outcome = verify_function(function, program, &mut session);
        let duration_us = started.elapsed().as_micros() as u64;
        let (status, message, constraint_count, domain) = match outcome {
            Ok(outcome) => outcome,
            Err(message) => (
                VerifStatus::NotInTrustedSubset,
                message,
                0,
                Some(TrustedSubsetDomain::Body),
            ),
        };
        let artifact = if status.is_definitive() || status == VerifStatus::NoObligations {
            Some(ProofArtifact {
                semantics_version: ProofArtifact::SEMANTICS_VERSION,
                integer_model: "checked_i32_i64".into(),
                float_model: "f64_rejected".into(),
                solver_version: format!("z3 {}", z3::full_version()),
                source_hash: source_hash.clone(),
                resolved_ir_hash: String::new(),
                mir_hash: mir_hash.clone(),
                vir_hash: String::new(),
                engine: ProofArtifact::ENGINE_MIR.to_string(),
            })
        } else {
            None
        };
        results.push(VerificationResult {
            func_name: function.owner.0.clone(),
            status,
            message,
            diagnostic: None,
            duration_us,
            constraint_count,
            artifact,
            trusted_subset_domain: domain,
        });
    }

    Ok(results)
}

fn canonical_mir_hash(program: &MirProgram) -> String {
    let mut text = String::new();
    text.push_str("mimi-canonical-mir-verifier-v1\n");
    text.push_str(&program.type_catalog().canonical_text());
    for function in program.functions().values() {
        text.push_str(&function.canonical_text());
    }
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn verify_function(
    function: &MirFunction,
    program: &MirProgram,
    session: &mut SolverSession,
) -> Result<(VerifStatus, String, usize, Option<TrustedSubsetDomain>), String> {
    let catalog = program.type_catalog();
    let mut requires = Vec::new();
    let mut ensures = Vec::new();
    for contract in &function.contracts {
        match contract.kind {
            MirContractKind::Requires => requires.push(&contract.condition),
            MirContractKind::Ensures => ensures.push(&contract.condition),
            MirContractKind::Invariant => {
                return Ok((
                    VerifStatus::NotInTrustedSubset,
                    "canonical MIR verifier does not yet materialize invariant effect semantics"
                        .into(),
                    0,
                    Some(TrustedSubsetDomain::Contract),
                ));
            }
        }
    }
    if ensures.is_empty() {
        return Ok((
            VerifStatus::NoObligations,
            "canonical MIR verifier: no ensures contract".into(),
            0,
            None,
        ));
    }

    let mut initial = initial_state(function, catalog, session)?;
    let mut require_terms = Vec::with_capacity(requires.len());
    for condition in &requires {
        let term = contract_term(condition, &initial.values, &initial.values, None)?;
        require_terms.push(expect_bool(term, "requires contract")?);
    }
    initial.constraints.extend(require_terms.iter().cloned());

    let mut returns = Vec::new();
    let mut traps = Vec::new();
    explore_block(
        function,
        catalog,
        &mut initial,
        &function.entry,
        &mut BTreeSet::new(),
        &mut returns,
        &mut traps,
    )?;
    if returns.is_empty() {
        return Err("canonical MIR body has no non-trapping return path".into());
    }

    let mut constraint_count = require_terms.len();
    let mut saw_unknown = false;

    // A trapping arithmetic operation is a real MIR execution path.  It may
    // only be omitted from the proof when the requires clause excludes it.
    for trap in traps {
        let condition = conjunction(&trap.condition);
        constraint_count += trap.condition.len();
        match session.check_scope(condition) {
            (SatResult::Sat, _) => {
                return Ok((
                    VerifStatus::Disproven,
                    format!(
                        "canonical MIR body can reach trap '{}' under requires",
                        trap.code
                    ),
                    constraint_count,
                    Some(TrustedSubsetDomain::Body),
                ));
            }
            (SatResult::Unknown, _) => saw_unknown = true,
            (SatResult::Unsat, _) => {}
        }
    }
    if saw_unknown {
        return Ok((
            session.unknown_status(),
            "canonical MIR verifier could not discharge a trap path".into(),
            constraint_count,
            Some(TrustedSubsetDomain::Body),
        ));
    }

    for path in returns {
        let path_condition = conjunction(&path.constraints);
        for ensure in &ensures {
            let term = contract_term(
                ensure,
                &path_value_map(&path),
                &initial.values,
                Some(&path.value),
            )?;
            let condition = expect_bool(term, "ensures contract")?;
            let violation = Bool::and(&[&path_condition, &condition.not()]);
            constraint_count += 1;
            match session.check_scope(violation) {
                (SatResult::Sat, _) => {
                    return Ok((
                        VerifStatus::Disproven,
                        "canonical MIR ensures contract is disproven".into(),
                        constraint_count,
                        Some(TrustedSubsetDomain::Contract),
                    ));
                }
                (SatResult::Unknown, _) => saw_unknown = true,
                (SatResult::Unsat, _) => {}
            }
        }
    }
    if saw_unknown {
        Ok((
            session.unknown_status(),
            "canonical MIR verifier could not discharge an ensures contract".into(),
            constraint_count,
            Some(TrustedSubsetDomain::Contract),
        ))
    } else {
        Ok((
            VerifStatus::Proven,
            "canonical MIR ensures contract proven".into(),
            constraint_count,
            None,
        ))
    }
}

// Return paths retain their complete SSA environment in the constraints by
// using the same value IDs.  This helper makes the contract API explicit and
// keeps the verifier from reaching back into a frontend body.
fn path_value_map(path: &ReturnPath) -> BTreeMap<MirValueId, SymbolicValue> {
    path.values.clone()
}

fn initial_state(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    session: &mut SolverSession,
) -> Result<SymbolicState, String> {
    let mut state = SymbolicState {
        values: BTreeMap::new(),
        constraints: Vec::new(),
        traps: Vec::new(),
    };
    for parameter in &function.parameters {
        let name = format!("mir.value.{}", parameter.as_str());
        let (value, constraints) = symbolic_value_for_type(
            catalog,
            &function
                .values
                .get(parameter)
                .ok_or_else(|| format!("MIR parameter '{}' is absent from values", parameter))?
                .ty,
            &name,
        )?;
        state.constraints.extend(constraints);
        state.values.insert(parameter.clone(), value);
    }
    // Keep the session argument in the constructor signature so all future
    // canonical initialization constraints have one explicit proof boundary.
    let _ = session;
    Ok(state)
}

fn symbolic_value_for_type(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ir::ResolvedTypeId,
    name: &str,
) -> Result<(SymbolicValue, Vec<Bool>), String> {
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| format!("MIR verifier TypeDesc '{}' is absent", ty.as_str()))?;
    if descriptor.ownership != MirOwnership::Copy
        || descriptor.glue.move_out != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.glue.clone != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.glue.drop != crate::core::mir::types::MirGlueKind::Noop
    {
        return Err(format!(
            "MIR verifier TypeDesc '{}' is outside the Copy/no-op aggregate contract",
            ty.as_str()
        ));
    }
    match &descriptor.layout {
        MirLayout::Scalar => match descriptor.abi {
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } => {
                let MirAbiClass::Integer { bits, .. } = descriptor.abi else {
                    unreachable!()
                };
                let symbol = Int::new_const(name);
                Ok((
                    SymbolicValue::Int(symbol.clone()),
                    vec![int_range_constraint(&symbol, bits)],
                ))
            }
            MirAbiClass::Bool => Ok((SymbolicValue::Bool(Bool::new_const(name)), Vec::new())),
            abi => Err(format!(
                "MIR verifier ABI {:?} is outside the checked scalar contract",
                abi
            )),
        },
        MirLayout::Tuple(elements) => {
            let mut values = Vec::with_capacity(elements.len());
            let mut constraints = Vec::new();
            for (index, element) in elements.iter().enumerate() {
                let (value, nested) =
                    symbolic_value_for_type(catalog, element, &format!("{name}.tuple{index}"))?;
                values.push(value);
                constraints.extend(nested);
            }
            Ok((SymbolicValue::Tuple(values), constraints))
        }
        MirLayout::Record { nominal, fields } => {
            let mut values = BTreeMap::new();
            let mut constraints = Vec::new();
            for field in fields {
                let (value, nested) = symbolic_value_for_type(
                    catalog,
                    &field.ty,
                    &format!("{name}.field.{}", field.id.0.as_str()),
                )?;
                values.insert(field.id.clone(), value);
                constraints.extend(nested);
            }
            Ok((
                SymbolicValue::Record {
                    nominal: nominal.clone(),
                    fields: values,
                },
                constraints,
            ))
        }
        MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => {
            let expected_nominal = if matches!(&descriptor.layout, MirLayout::Option { .. }) {
                "builtin:type:Option"
            } else {
                "builtin:type:Result"
            };
            let tag = Int::new_const(format!("{name}.tag"));
            let mut constraints = Vec::new();
            let allowed = variants
                .iter()
                .map(|variant| tag.eq(Int::from_i64(variant.discriminant as i64)))
                .collect::<Vec<_>>();
            if !allowed.is_empty() {
                let allowed_refs = allowed.iter().collect::<Vec<_>>();
                constraints.push(Bool::or(&allowed_refs));
            }
            let mut payload = BTreeMap::new();
            for variant in variants {
                for field in &variant.fields {
                    let (value, nested) = symbolic_value_for_type(
                        catalog,
                        &field.ty,
                        &format!(
                            "{name}.variant.{}.field.{}",
                            variant.id.0.as_str(),
                            field.id.0.as_str()
                        ),
                    )?;
                    if payload.insert(field.id.clone(), value).is_some() {
                        return Err(format!(
                            "MIR verifier variant payload field '{}' is duplicated",
                            field.id.0
                        ));
                    }
                    constraints.extend(nested);
                }
            }
            Ok((
                SymbolicValue::Variant {
                    nominal: crate::core::ir::NominalTypeId::new(expected_nominal)
                        .map_err(|error| error.to_string())?,
                    tag,
                    payload,
                },
                constraints,
            ))
        }
        layout => Err(format!(
            "MIR verifier layout {:?} is outside the Copy aggregate contract",
            layout
        )),
    }
}

fn value_scalar_kind(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    value: &MirValueId,
) -> Result<ScalarKind, String> {
    let info = function
        .values
        .get(value)
        .ok_or_else(|| format!("MIR verifier value '{}' is absent", value))?;
    let descriptor = catalog
        .get(&info.ty)
        .ok_or_else(|| format!("MIR verifier TypeDesc '{}' is absent", info.ty.as_str()))?;
    if descriptor.layout != MirLayout::Scalar
        || descriptor.ownership != MirOwnership::Copy
        || descriptor.glue.move_out != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.glue.clone != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.glue.drop != crate::core::mir::types::MirGlueKind::Noop
    {
        return Err(format!(
            "MIR verifier value '{}' is outside the Copy scalar TypeDesc/glue contract",
            value
        ));
    }
    match descriptor.abi {
        MirAbiClass::Integer {
            bits: 32 | 64,
            signed: true,
        } => match descriptor.abi {
            MirAbiClass::Integer { bits, .. } => Ok(ScalarKind::Int { bits }),
            _ => unreachable!(),
        },
        MirAbiClass::Bool => Ok(ScalarKind::Bool),
        abi => Err(format!(
            "MIR verifier ABI {:?} is outside the checked scalar contract",
            abi
        )),
    }
}

fn int_range_constraint(value: &Int, bits: u16) -> Bool {
    let (lo, hi) = if bits == 32 {
        (i32::MIN as i64, i32::MAX as i64)
    } else {
        (i64::MIN, i64::MAX)
    };
    Bool::and(&[&value.ge(Int::from_i64(lo)), &value.le(Int::from_i64(hi))])
}

fn resolved_projection(
    projection: &crate::core::ir::ResolvedProjection,
) -> Result<MirProjection, String> {
    match projection {
        crate::core::ir::ResolvedProjection::Field { field, .. } => {
            Ok(MirProjection::Field(field.clone()))
        }
        crate::core::ir::ResolvedProjection::Tuple { index, .. } => {
            Ok(MirProjection::Tuple(*index))
        }
        crate::core::ir::ResolvedProjection::Index { .. } => {
            Err("MIR verifier does not admit indexed contract projection".into())
        }
        crate::core::ir::ResolvedProjection::Deref { .. } => {
            Err("MIR verifier does not admit dereference contract projection".into())
        }
    }
}

fn symbolic_project(
    value: SymbolicValue,
    projection: &MirProjection,
) -> Result<SymbolicValue, String> {
    match (value, projection) {
        (SymbolicValue::Tuple(values), MirProjection::Tuple(index)) => values
            .get(*index)
            .cloned()
            .ok_or_else(|| format!("MIR tuple projection index {} is out of bounds", index)),
        (SymbolicValue::Record { fields, .. }, MirProjection::Field(field)) => fields
            .get(field)
            .cloned()
            .ok_or_else(|| format!("MIR record projection field '{}' is absent", field.0)),
        (_, MirProjection::Index(_) | MirProjection::Dereference) => {
            Err("MIR verifier projection is outside the aggregate contract".into())
        }
        _ => Err("MIR verifier projection base is not an aggregate".into()),
    }
}

fn symbolic_variant_construct(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    result_ty: &crate::core::ir::ResolvedTypeId,
    nominal: &crate::core::ir::NominalTypeId,
    variant: &crate::core::NodeId,
    fields: &[(crate::core::NodeId, SymbolicValue)],
) -> Result<SymbolicValue, String> {
    let Some((expected_nominal, variants)) = catalog.variant_layout(result_ty) else {
        return Err("MIR verifier variant construction has no canonical layout".into());
    };
    if nominal.as_str() != expected_nominal {
        return Err("MIR verifier variant construction nominal disagrees with TypeDesc".into());
    }
    let Some(expected_variant) = variants.iter().find(|candidate| candidate.id == *variant) else {
        return Err("MIR verifier variant construction case is absent from TypeDesc".into());
    };
    if expected_variant.fields.len() != fields.len() {
        return Err(
            "MIR verifier variant construction payload arity disagrees with TypeDesc".into(),
        );
    }
    let mut payload = BTreeMap::new();
    for (field_id, value) in fields {
        let Some(expected_field) = expected_variant
            .fields
            .iter()
            .find(|field| field.id == *field_id)
        else {
            return Err("MIR verifier variant construction field is absent from TypeDesc".into());
        };
        if !symbolic_matches_type(catalog, &expected_field.ty, value) {
            return Err("MIR verifier variant payload disagrees with TypeDesc".into());
        }
        if payload.insert(field_id.clone(), value.clone()).is_some() {
            return Err("MIR verifier variant construction repeats a field".into());
        }
    }
    if payload.len() != expected_variant.fields.len() {
        return Err("MIR verifier variant construction is missing a payload field".into());
    }
    Ok(SymbolicValue::Variant {
        nominal: crate::core::ir::NominalTypeId::new(expected_nominal)
            .map_err(|error| error.to_string())?,
        tag: Int::from_i64(expected_variant.discriminant as i64),
        payload,
    })
}

fn symbolic_default_guard(previous_cases: &[Bool]) -> Bool {
    if previous_cases.is_empty() {
        return Bool::from_bool(true);
    }
    let negated = previous_cases
        .iter()
        .map(|condition| condition.not())
        .collect::<Vec<_>>();
    let refs = negated.iter().collect::<Vec<_>>();
    Bool::and(&refs)
}

fn explore_block(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    block_id: &crate::core::mir::MirBlockId,
    active: &mut BTreeSet<crate::core::mir::MirBlockId>,
    returns: &mut Vec<ReturnPath>,
    traps: &mut Vec<SymbolicTrap>,
) -> Result<(), String> {
    if !active.insert(block_id.clone()) {
        return Err("canonical MIR verifier does not yet admit cyclic CFG/loops".into());
    }
    let block = function
        .blocks
        .get(block_id)
        .ok_or_else(|| format!("MIR verifier block '{}' is absent", block_id))?;
    for instruction in &block.instructions {
        eval_instruction(function, catalog, state, &instruction.kind)?;
    }
    match &block.terminator {
        MirTerminator::Goto {
            target, arguments, ..
        } => {
            let mut next = edge_state(state, function, target, arguments)?;
            explore_block(function, catalog, &mut next, target, active, returns, traps)?;
        }
        MirTerminator::Branch {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
            ..
        } => {
            let condition =
                expect_bool(
                    state.values.get(condition).cloned().ok_or_else(|| {
                        format!("branch condition '{}' is not defined", condition)
                    })?,
                    "branch condition",
                )?;
            let mut then_state = edge_state(state, function, then_target, then_arguments)?;
            then_state.constraints.push(condition.clone());
            explore_block(
                function,
                catalog,
                &mut then_state,
                then_target,
                &mut active.clone(),
                returns,
                traps,
            )?;
            let mut else_state = edge_state(state, function, else_target, else_arguments)?;
            else_state.constraints.push(condition.not());
            explore_block(
                function,
                catalog,
                &mut else_state,
                else_target,
                &mut active.clone(),
                returns,
                traps,
            )?;
        }
        MirTerminator::Switch { scrutinee, arms } => {
            let value = state
                .values
                .get(scrutinee)
                .cloned()
                .ok_or_else(|| format!("switch scrutinee '{}' is not defined", scrutinee))?;
            let SymbolicValue::Variant {
                nominal,
                tag,
                payload,
            } = value
            else {
                return Err(
                    "canonical MIR verifier variant switch requires a symbolic Option/Result value"
                        .into(),
                );
            };
            let scrutinee_ty = function
                .values
                .get(scrutinee)
                .map(|value| value.ty.clone())
                .ok_or_else(|| format!("switch scrutinee '{}' has no TypeDesc", scrutinee))?;
            let Some((expected_nominal, variants)) = catalog.variant_layout(&scrutinee_ty) else {
                return Err(
                    "canonical MIR verifier variant switch has no canonical TypeDesc layout".into(),
                );
            };
            if nominal.as_str() != expected_nominal {
                return Err(
                    "canonical MIR verifier variant switch nominal disagrees with TypeDesc".into(),
                );
            }
            let mut previous_cases = Vec::new();
            for arm in arms {
                let (guard, bindings) = match &arm.case {
                    MirSwitchCase::Variant(variant_id) => {
                        let variant = variants
                            .iter()
                            .find(|variant| variant.id == *variant_id)
                            .ok_or_else(|| {
                                format!(
                                    "canonical MIR verifier switch variant '{}' is absent from TypeDesc",
                                    variant_id.0
                                )
                            })?;
                        let guard = tag.eq(Int::from_i64(variant.discriminant as i64));
                        previous_cases.push(guard.clone());
                        let mut bindings = Vec::new();
                        for binding in &arm.bindings {
                            let field = variant
                                .fields
                                .iter()
                                .find(|field| field.id == binding.field)
                                .ok_or_else(|| {
                                    format!(
                                        "canonical MIR verifier switch binding field '{}' is absent from TypeDesc",
                                        binding.field.0
                                    )
                                })?;
                            let value = payload.get(&field.id).cloned().ok_or_else(|| {
                                format!(
                                    "canonical MIR verifier switch payload field '{}' is absent",
                                    field.id.0
                                )
                            })?;
                            let parameter = function
                                .values
                                .get(&binding.parameter)
                                .ok_or_else(|| {
                                    format!(
                                        "canonical MIR verifier switch binding parameter '{}' is absent",
                                        binding.parameter
                                    )
                                })?;
                            if !symbolic_matches_type(catalog, &parameter.ty, &value) {
                                return Err(format!(
                                    "canonical MIR verifier switch binding '{}' disagrees with payload TypeDesc",
                                    binding.parameter
                                ));
                            }
                            bindings.push((binding.parameter.clone(), value));
                        }
                        (guard, bindings)
                    }
                    MirSwitchCase::Default => {
                        (symbolic_default_guard(&previous_cases), Vec::new())
                    }
                    MirSwitchCase::Literal(_) => {
                        return Err(
                            "canonical MIR verifier variant switch cannot use a literal case".into(),
                        )
                    }
                };
                let mut next = edge_state(state, function, &arm.target, &arm.arguments)?;
                next.constraints.push(guard);
                for (parameter, value) in bindings {
                    next.values.insert(parameter, value);
                }
                explore_block(
                    function,
                    catalog,
                    &mut next,
                    &arm.target,
                    &mut active.clone(),
                    returns,
                    traps,
                )?;
            }
        }
        MirTerminator::Return { value } => {
            let value = value
                .as_ref()
                .and_then(|value| state.values.get(value).cloned())
                .ok_or_else(|| "MIR verifier return value is absent".to_string())?;
            returns.push(ReturnPath {
                constraints: state.constraints.clone(),
                values: state.values.clone(),
                value,
            });
            traps.extend(state.traps.clone());
        }
        MirTerminator::Trap { code } => {
            traps.push(SymbolicTrap {
                condition: state.constraints.clone(),
                code: code.clone(),
            });
        }
        MirTerminator::Unreachable => {}
        MirTerminator::SwitchMove { .. }
        | MirTerminator::Fault { .. } => {
            return Err(
                "canonical MIR verifier currently supports scalar Goto/Branch and Copy variant Switch CFG".into(),
            )
        }
    }
    active.remove(block_id);
    Ok(())
}

fn edge_state(
    state: &SymbolicState,
    function: &MirFunction,
    target: &crate::core::mir::MirBlockId,
    arguments: &[MirValueId],
) -> Result<SymbolicState, String> {
    let block = function
        .blocks
        .get(target)
        .ok_or_else(|| format!("MIR verifier target block '{}' is absent", target))?;
    let mut next = state.clone();
    for (parameter, argument) in block.parameters.iter().zip(arguments) {
        let value =
            state.values.get(argument).cloned().ok_or_else(|| {
                format!("MIR verifier edge argument '{}' is not defined", argument)
            })?;
        next.values.insert(parameter.value.clone(), value);
    }
    Ok(next)
}

fn eval_instruction(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    instruction: &MirInstructionKind,
) -> Result<(), String> {
    match instruction {
        MirInstructionKind::Const { result, literal } => {
            let kind = value_scalar_kind(function, catalog, result)?;
            let value = match (kind, literal) {
                (ScalarKind::Int { .. }, crate::core::ir::ResolvedLiteral::Int(value)) => {
                    SymbolicValue::Int(Int::from_i64(*value))
                }
                (ScalarKind::Bool, crate::core::ir::ResolvedLiteral::Bool(value)) => {
                    SymbolicValue::Bool(Bool::from_bool(*value))
                }
                _ => return Err("MIR scalar const literal disagrees with TypeDesc ABI".into()),
            };
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Load { result, place } => {
            let source = MirValueId::new(format!("local:{}", place.base.0 .0))
                .map_err(|error| error.to_string())?;
            let mut value = state
                .values
                .get(&source)
                .cloned()
                .ok_or_else(|| format!("MIR load source '{}' is not defined", source))?;
            for projection in &place.projections {
                let projection = resolved_projection(projection)?;
                value = symbolic_project(value, &projection)?;
            }
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Copy { result, source }
        | MirInstructionKind::Move { result, source }
        | MirInstructionKind::Clone { result, source } => {
            let value = state
                .values
                .get(source)
                .cloned()
                .ok_or_else(|| format!("MIR value '{}' is not defined", source))?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Drop { value } => {
            ensure_copy_value(function, catalog, value)?;
            if !state.values.contains_key(value) {
                return Err(format!("MIR drop value '{}' is not defined", value));
            }
        }
        MirInstructionKind::Unary {
            result,
            op,
            operand,
        } => {
            let value = state
                .values
                .get(operand)
                .cloned()
                .ok_or_else(|| format!("MIR unary operand '{}' is not defined", operand))?;
            let output = match (op, value) {
                (crate::core::ir::ResolvedUnaryOp::Negate, SymbolicValue::Int(value)) => {
                    let kind = value_scalar_kind(function, catalog, result)?;
                    let ScalarKind::Int { bits } = kind else {
                        return Err("MIR negate result is not an integer TypeDesc".into());
                    };
                    let defined = value.ne(Int::from_i64(if bits == 32 {
                        i32::MIN as i64
                    } else {
                        i64::MIN
                    }));
                    add_definedness(state, defined, "E0802")?;
                    SymbolicValue::Int(value.unary_minus())
                }
                (crate::core::ir::ResolvedUnaryOp::Not, SymbolicValue::Bool(value)) => {
                    SymbolicValue::Bool(value.not())
                }
                _ => return Err("MIR unary operation is outside scalar verifier contract".into()),
            };
            ensure_result_shape(function, catalog, result, &output)?;
            state.values.insert(result.clone(), output);
        }
        MirInstructionKind::Binary {
            result,
            op,
            left,
            right,
        } => {
            let left = state
                .values
                .get(left)
                .cloned()
                .ok_or_else(|| format!("MIR binary left value '{}' is not defined", left))?;
            let right = state
                .values
                .get(right)
                .cloned()
                .ok_or_else(|| format!("MIR binary right value '{}' is not defined", right))?;
            let output = eval_binary(function, catalog, state, *op, left, right, result)?;
            ensure_result_shape(function, catalog, result, &output)?;
            state.values.insert(result.clone(), output);
        }
        MirInstructionKind::BuiltinCall {
            result,
            kind,
            arguments,
        } => {
            let args =
                arguments
                    .iter()
                    .map(|value| {
                        state.values.get(value).cloned().ok_or_else(|| {
                            format!("MIR builtin argument '{}' is not defined", value)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            let output = match (kind, args.as_slice()) {
                (MirBuiltinKind::Abs, [SymbolicValue::Int(value)]) => {
                    add_definedness(state, value.ne(Int::from_i64(i64::MIN)), "E0802")?;
                    SymbolicValue::Int(value.ge(Int::from_i64(0)).ite(value, &value.unary_minus()))
                }
                (MirBuiltinKind::Min, [SymbolicValue::Int(left), SymbolicValue::Int(right)]) => {
                    SymbolicValue::Int(left.le(right).ite(left, right))
                }
                (MirBuiltinKind::Max, [SymbolicValue::Int(left), SymbolicValue::Int(right)]) => {
                    SymbolicValue::Int(left.ge(right).ite(left, right))
                }
                _ => return Err("MIR builtin is outside scalar verifier contract".into()),
            };
            ensure_result_shape(function, catalog, result, &output)?;
            state.values.insert(result.clone(), output);
        }
        MirInstructionKind::Convert { result, source } => {
            let value = state
                .values
                .get(source)
                .cloned()
                .ok_or_else(|| format!("MIR conversion source '{}' is not defined", source))?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Nop => {}
        MirInstructionKind::Project {
            result,
            base,
            projection,
        } => {
            let value = state
                .values
                .get(base)
                .cloned()
                .ok_or_else(|| format!("MIR projection base '{}' is not defined", base))?;
            let value = symbolic_project(value, projection)?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Construct {
            result,
            kind,
            fields,
        } => {
            let values =
                fields
                    .iter()
                    .map(|value| {
                        state.values.get(value).cloned().ok_or_else(|| {
                            format!("MIR aggregate field '{}' is not defined", value)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            let value = symbolic_construct(function, catalog, result, kind, values)?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::ConstructVariant {
            result,
            nominal,
            variant,
            fields,
        } => {
            let values = fields
                .iter()
                .map(|(field, value)| {
                    state
                        .values
                        .get(value)
                        .cloned()
                        .map(|value| (field.clone(), value))
                        .ok_or_else(|| {
                            format!("MIR variant payload value '{}' is not defined", value)
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let value = symbolic_variant_construct(
                catalog,
                &function
                    .values
                    .get(result)
                    .ok_or_else(|| format!("MIR variant result '{}' is absent", result))?
                    .ty,
                nominal,
                variant,
                &values,
            )?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::MoveProject { .. }
        | MirInstructionKind::ConstructList { .. }
        | MirInstructionKind::Borrow { .. }
        | MirInstructionKind::EndBorrow { .. }
        | MirInstructionKind::ConstructVariantMove { .. }
        | MirInstructionKind::Call { .. } => {
            return Err("MIR instruction is outside scalar verifier contract".into())
        }
        MirInstructionKind::UpdateRecord {
            result,
            base,
            kind: MirAggregateKind::Record { nominal, fields },
            fields: update_values,
        } => {
            ensure_copy_value(function, catalog, base)?;
            for value in update_values {
                ensure_copy_value(function, catalog, value)?;
            }
            let base_value = state
                .values
                .get(base)
                .cloned()
                .ok_or_else(|| format!("MIR record update base '{}' is not defined", base))?;
            let update_values = update_values
                .iter()
                .map(|value| {
                    state.values.get(value).cloned().ok_or_else(|| {
                        format!("MIR record update value '{}' is not defined", value)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let value = symbolic_update_record(
                function,
                catalog,
                result,
                base_value,
                nominal,
                fields,
                update_values,
            )?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::UpdateRecord { .. } => {
            return Err("MIR record update requires a record aggregate kind".into())
        }
    }
    Ok(())
}

fn ensure_copy_value(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    value: &MirValueId,
) -> Result<(), String> {
    let info = function
        .values
        .get(value)
        .ok_or_else(|| format!("MIR value '{}' is absent", value))?;
    let descriptor = catalog
        .get(&info.ty)
        .ok_or_else(|| format!("MIR value '{}' TypeDesc is absent", value))?;
    if descriptor.ownership != MirOwnership::Copy
        || descriptor.glue.move_out != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.glue.clone != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.glue.drop != crate::core::mir::types::MirGlueKind::Noop
        || !matches!(
            descriptor.layout,
            MirLayout::Scalar
                | MirLayout::Tuple(_)
                | MirLayout::Record { .. }
                | MirLayout::Option { .. }
                | MirLayout::Result { .. }
        )
    {
        return Err(format!(
            "MIR value '{}' is outside the Copy/no-op contract",
            value
        ));
    }
    Ok(())
}

fn ensure_result_shape(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    result: &MirValueId,
    value: &SymbolicValue,
) -> Result<(), String> {
    let ty = function
        .values
        .get(result)
        .ok_or_else(|| format!("MIR result '{}' is absent", result))?
        .ty
        .clone();
    if symbolic_matches_type(catalog, &ty, value) {
        Ok(())
    } else {
        Err(format!(
            "MIR result '{}' disagrees with canonical TypeDesc shape",
            result
        ))
    }
}

fn symbolic_matches_type(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ir::ResolvedTypeId,
    value: &SymbolicValue,
) -> bool {
    let Some(descriptor) = catalog.get(ty) else {
        return false;
    };
    match (&descriptor.layout, &descriptor.abi, value) {
        (
            MirLayout::Scalar,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            },
            SymbolicValue::Int(_),
        )
        | (MirLayout::Scalar, MirAbiClass::Bool, SymbolicValue::Bool(_)) => true,
        (MirLayout::Tuple(elements), _, SymbolicValue::Tuple(values)) => {
            elements.len() == values.len()
                && elements
                    .iter()
                    .zip(values)
                    .all(|(ty, value)| symbolic_matches_type(catalog, ty, value))
        }
        (
            MirLayout::Record { nominal, fields },
            _,
            SymbolicValue::Record {
                nominal: actual_nominal,
                fields: actual_fields,
            },
        ) => {
            nominal == actual_nominal
                && fields.len() == actual_fields.len()
                && fields.iter().all(|field| {
                    actual_fields
                        .get(&field.id)
                        .is_some_and(|value| symbolic_matches_type(catalog, &field.ty, value))
                })
        }
        (
            MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. },
            MirAbiClass::Aggregate,
            SymbolicValue::Variant {
                nominal: actual_nominal,
                tag: _,
                payload,
            },
        ) => {
            let expected_nominal = if matches!(&descriptor.layout, MirLayout::Option { .. }) {
                "builtin:type:Option"
            } else {
                "builtin:type:Result"
            };
            actual_nominal.as_str() == expected_nominal
                && payload.len()
                    == variants
                        .iter()
                        .map(|variant| variant.fields.len())
                        .sum::<usize>()
                && variants
                    .iter()
                    .flat_map(|variant| &variant.fields)
                    .all(|field| {
                        payload
                            .get(&field.id)
                            .is_some_and(|value| symbolic_matches_type(catalog, &field.ty, value))
                    })
        }
        _ => false,
    }
}

fn symbolic_construct(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    result: &MirValueId,
    kind: &MirAggregateKind,
    values: Vec<SymbolicValue>,
) -> Result<SymbolicValue, String> {
    let result_ty = function
        .values
        .get(result)
        .ok_or_else(|| format!("MIR aggregate result '{}' is absent", result))?
        .ty
        .clone();
    let descriptor = catalog
        .get(&result_ty)
        .ok_or_else(|| format!("MIR aggregate result '{}' TypeDesc is absent", result))?;
    match (kind, &descriptor.layout) {
        (MirAggregateKind::Tuple, MirLayout::Tuple(elements)) if elements.len() == values.len() => {
            Ok(SymbolicValue::Tuple(values))
        }
        (
            MirAggregateKind::Record {
                nominal,
                fields: field_ids,
            },
            MirLayout::Record {
                nominal: expected_nominal,
                fields: layout_fields,
            },
        ) if nominal == expected_nominal
            && field_ids.len() == values.len()
            && field_ids.len() == layout_fields.len() =>
        {
            let mut fields = BTreeMap::new();
            for (field, value) in field_ids.iter().cloned().zip(values) {
                if fields.insert(field, value).is_some() {
                    return Err("MIR record construction repeats a field".into());
                }
            }
            Ok(SymbolicValue::Record {
                nominal: expected_nominal.clone(),
                fields,
            })
        }
        _ => Err("MIR aggregate construction disagrees with canonical TypeDesc".into()),
    }
}

fn symbolic_update_record(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    result: &MirValueId,
    base: SymbolicValue,
    nominal: &crate::core::ir::NominalTypeId,
    fields: &[crate::core::NodeId],
    update_values: Vec<SymbolicValue>,
) -> Result<SymbolicValue, String> {
    ensure_copy_value(function, catalog, result)?;

    let result_ty = function
        .values
        .get(result)
        .ok_or_else(|| format!("MIR record update result '{}' is absent", result))?
        .ty
        .clone();
    let descriptor = catalog
        .get(&result_ty)
        .ok_or_else(|| format!("MIR record update result '{}' TypeDesc is absent", result))?;
    let MirLayout::Record {
        nominal: expected_nominal,
        fields: layout_fields,
    } = &descriptor.layout
    else {
        return Err("MIR record update result has no canonical record layout".into());
    };
    let SymbolicValue::Record {
        nominal: base_nominal,
        fields: mut base_fields,
    } = base
    else {
        return Err("MIR record update base is not a symbolic record".into());
    };
    if nominal != expected_nominal || &base_nominal != expected_nominal {
        return Err("MIR record update nominal disagrees with TypeDesc".into());
    }
    if fields.len() != update_values.len() {
        return Err("MIR record update field/value arity disagrees".into());
    }
    if base_fields.len() != layout_fields.len()
        || layout_fields
            .iter()
            .any(|field| !base_fields.contains_key(&field.id))
    {
        return Err("MIR record update base disagrees with TypeDesc fields".into());
    }

    let mut seen = BTreeSet::new();
    for (field, value) in fields.iter().zip(update_values) {
        if !seen.insert(field) {
            return Err(format!("MIR record update repeats field '{}'", field.0));
        }
        let expected = layout_fields
            .iter()
            .find(|candidate| candidate.id == *field)
            .ok_or_else(|| format!("MIR record update field '{}' is absent", field.0))?;
        if !symbolic_matches_type(catalog, &expected.ty, &value) {
            return Err(format!(
                "MIR record update field '{}' disagrees with TypeDesc",
                field.0
            ));
        }
        base_fields.insert(field.clone(), value);
    }
    Ok(SymbolicValue::Record {
        nominal: expected_nominal.clone(),
        fields: base_fields,
    })
}

fn eval_binary(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    op: crate::core::ir::ResolvedBinaryOp,
    left: SymbolicValue,
    right: SymbolicValue,
    result: &MirValueId,
) -> Result<SymbolicValue, String> {
    use crate::core::ir::ResolvedBinaryOp as Op;
    match (left, right) {
        (SymbolicValue::Int(left), SymbolicValue::Int(right)) => {
            let output = match op {
                Op::Add => Int::add(&[&left, &right]),
                Op::Subtract => Int::sub(&[&left, &right]),
                Op::Multiply => Int::mul(&[&left, &right]),
                Op::Divide | Op::Remainder => {
                    let kind = value_scalar_kind(function, catalog, result)?;
                    let ScalarKind::Int { bits } = kind else {
                        return Err("MIR integer operation has non-integer result TypeDesc".into());
                    };
                    let zero = Int::from_i64(0);
                    let min = Int::from_i64(if bits == 32 {
                        i32::MIN as i64
                    } else {
                        i64::MIN
                    });
                    let neg_one = Int::from_i64(-1);
                    let defined = Bool::and(&[
                        &right.ne(&zero),
                        &Bool::and(&[&left.eq(&min), &right.eq(&neg_one)]).not(),
                    ]);
                    add_definedness(state, defined, "E0802")?;
                    let abs_left = left.ge(&zero).ite(&left, &left.unary_minus());
                    let abs_right = right.ge(&zero).ite(&right, &right.unary_minus());
                    let quotient = abs_left.div(&abs_right);
                    let remainder = abs_left.modulo(&abs_right);
                    let same_sign = left.ge(&zero).eq(&right.ge(&zero));
                    if op == Op::Divide {
                        same_sign.ite(&quotient, &quotient.unary_minus())
                    } else {
                        left.ge(&zero).ite(&remainder, &remainder.unary_minus())
                    }
                }
                Op::Equal => return Ok(SymbolicValue::Bool(left.eq(&right))),
                Op::NotEqual => return Ok(SymbolicValue::Bool(left.eq(&right).not())),
                Op::Less => return Ok(SymbolicValue::Bool(left.lt(&right))),
                Op::Greater => return Ok(SymbolicValue::Bool(left.gt(&right))),
                Op::LessEqual => return Ok(SymbolicValue::Bool(left.le(&right))),
                Op::GreaterEqual => return Ok(SymbolicValue::Bool(left.ge(&right))),
                _ => return Err("MIR integer binary operation is outside verifier contract".into()),
            };
            if matches!(op, Op::Add | Op::Subtract | Op::Multiply) {
                let ScalarKind::Int { bits } = value_scalar_kind(function, catalog, result)? else {
                    return Err("MIR arithmetic result is not an integer TypeDesc".into());
                };
                add_definedness(state, int_range_constraint(&output, bits), "E0802")?;
            }
            Ok(SymbolicValue::Int(output))
        }
        (SymbolicValue::Bool(left), SymbolicValue::Bool(right)) => match op {
            Op::Equal => Ok(SymbolicValue::Bool(left.eq(&right))),
            Op::NotEqual => Ok(SymbolicValue::Bool(left.eq(&right).not())),
            Op::LogicalAnd => Ok(SymbolicValue::Bool(Bool::and(&[&left, &right]))),
            Op::LogicalOr => Ok(SymbolicValue::Bool(Bool::or(&[&left, &right]))),
            _ => Err("MIR boolean binary operation is outside verifier contract".into()),
        },
        _ => Err("MIR binary operands have incompatible scalar kinds".into()),
    }
}

fn add_definedness(state: &mut SymbolicState, defined: Bool, code: &str) -> Result<(), String> {
    let mut trap_condition = state.constraints.clone();
    trap_condition.push(defined.not());
    state.traps.push(SymbolicTrap {
        condition: trap_condition,
        code: code.into(),
    });
    state.constraints.push(defined);
    Ok(())
}

fn expect_bool(value: SymbolicValue, context: &str) -> Result<Bool, String> {
    match value {
        SymbolicValue::Bool(value) => Ok(value),
        SymbolicValue::Int(_)
        | SymbolicValue::Tuple(_)
        | SymbolicValue::Record { .. }
        | SymbolicValue::Variant { .. } => Err(format!("{context} is not boolean")),
    }
}

fn conjunction(conditions: &[Bool]) -> Bool {
    if conditions.is_empty() {
        Bool::from_bool(true)
    } else {
        let refs = conditions.iter().collect::<Vec<_>>();
        Bool::and(&refs)
    }
}

fn contract_term(
    expression: &MirContractExpr,
    values: &BTreeMap<MirValueId, SymbolicValue>,
    old_values: &BTreeMap<MirValueId, SymbolicValue>,
    result: Option<&SymbolicValue>,
) -> Result<SymbolicValue, String> {
    match expression {
        MirContractExpr::Value(value) => values.get(value).cloned().ok_or_else(|| {
            format!(
                "contract value '{}' is not available on this MIR path",
                value
            )
        }),
        MirContractExpr::Old(value) => old_values
            .get(value)
            .cloned()
            .ok_or_else(|| format!("old contract value '{}' is not available", value)),
        MirContractExpr::Result => result
            .cloned()
            .ok_or_else(|| "ensures result is not available before a return path".into()),
        MirContractExpr::Project { base, projection } => {
            let value = contract_term(base, values, old_values, result)?;
            symbolic_project(value, projection)
        }
        MirContractExpr::Int(value) => Ok(SymbolicValue::Int(Int::from_i64(*value))),
        MirContractExpr::Bool(value) => Ok(SymbolicValue::Bool(Bool::from_bool(*value))),
        MirContractExpr::Unary { op, operand } => {
            let operand = contract_term(operand, values, old_values, result)?;
            match (op, operand) {
                (MirContractUnaryOp::Negate, SymbolicValue::Int(value)) => {
                    Ok(SymbolicValue::Int(value.unary_minus()))
                }
                (MirContractUnaryOp::Not, SymbolicValue::Bool(value)) => {
                    Ok(SymbolicValue::Bool(value.not()))
                }
                _ => Err("contract unary expression has incompatible symbolic kind".into()),
            }
        }
        MirContractExpr::Binary { op, left, right } => {
            let left = contract_term(left, values, old_values, result)?;
            let right = contract_term(right, values, old_values, result)?;
            contract_binary(*op, left, right)
        }
    }
}

fn contract_binary(
    op: MirContractBinaryOp,
    left: SymbolicValue,
    right: SymbolicValue,
) -> Result<SymbolicValue, String> {
    match (left, right) {
        (SymbolicValue::Int(left), SymbolicValue::Int(right)) => match op {
            MirContractBinaryOp::Add => Ok(SymbolicValue::Int(Int::add(&[&left, &right]))),
            MirContractBinaryOp::Subtract => {
                Ok(SymbolicValue::Int(Int::sub(&[&left, &right])))
            }
            MirContractBinaryOp::Multiply => {
                Ok(SymbolicValue::Int(Int::mul(&[&left, &right])))
            }
            MirContractBinaryOp::Equal => Ok(SymbolicValue::Bool(left.eq(&right))),
            MirContractBinaryOp::NotEqual => Ok(SymbolicValue::Bool(left.eq(&right).not())),
            MirContractBinaryOp::Less => Ok(SymbolicValue::Bool(left.lt(&right))),
            MirContractBinaryOp::Greater => Ok(SymbolicValue::Bool(left.gt(&right))),
            MirContractBinaryOp::LessEqual => Ok(SymbolicValue::Bool(left.le(&right))),
            MirContractBinaryOp::GreaterEqual => Ok(SymbolicValue::Bool(left.ge(&right))),
            MirContractBinaryOp::Divide | MirContractBinaryOp::Remainder => Err(
                "division/remainder in a canonical contract is deferred until its trap contract is materialized".into(),
            ),
            MirContractBinaryOp::LogicalAnd | MirContractBinaryOp::LogicalOr => {
                Err("contract logical operator requires boolean operands".into())
            }
        },
        (SymbolicValue::Bool(left), SymbolicValue::Bool(right)) => match op {
            MirContractBinaryOp::Equal => Ok(SymbolicValue::Bool(left.eq(&right))),
            MirContractBinaryOp::NotEqual => Ok(SymbolicValue::Bool(left.eq(&right).not())),
            MirContractBinaryOp::LogicalAnd => Ok(SymbolicValue::Bool(Bool::and(&[&left, &right]))),
            MirContractBinaryOp::LogicalOr => Ok(SymbolicValue::Bool(Bool::or(&[&left, &right]))),
            _ => Err("contract boolean operands do not support this operator".into()),
        },
        _ => Err("contract operands have incompatible symbolic kinds".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::verify_program;
    use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    #[test]
    fn verifier_and_reference_oracle_consume_the_same_canonical_mir() {
        let source = r#"
            func monotone_step(x: i32, choose_step: bool) -> i32 {
                requires: x < 2147483647
                ensures: result >= x
                if choose_step { x + 1 } else { x }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("monotone_step"))
            .cloned()
            .expect("monotone_step MIR function");

        let reference_value = MirReferenceInterpreter::new(&program)
            .execute(
                &owner,
                &[MirRuntimeValue::Int(41), MirRuntimeValue::Bool(true)],
            )
            .expect("reference execution");
        assert_eq!(reference_value, MirRuntimeValue::Int(42));

        let results = verify_program(&program, "source-hash".into()).expect("MIR verification");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("contract verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        let artifact = result.artifact.as_ref().expect("MIR proof artifact");
        assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.mir_hash.len(), 64);
        assert!(program
            .functions()
            .get(&owner)
            .expect("function")
            .canonical_text()
            .contains("contract"));
    }

    #[test]
    fn verifier_and_reference_oracle_preserve_copy_record_projection() {
        let source = r#"
            type Point { x: i32, enabled: bool }

            func advance(p: Point, choose_step: bool) -> Point {
                requires: p.x < 2147483647
                ensures: result.x == old(p.x) + 1
                if choose_step {
                    Point { x: p.x + 1, enabled: p.enabled }
                } else {
                    Point { x: p.x + 1, enabled: p.enabled }
                }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("advance"))
            .cloned()
            .expect("advance MIR function");
        let point = crate::core::ir::NominalTypeId::new("type:Point").expect("Point nominal");

        let reference_value = MirReferenceInterpreter::new(&program)
            .execute(
                &owner,
                &[
                    MirRuntimeValue::Record {
                        nominal: point,
                        fields: vec![MirRuntimeValue::Int(41), MirRuntimeValue::Bool(true)],
                    },
                    MirRuntimeValue::Bool(true),
                ],
            )
            .expect("reference record execution");
        assert_eq!(
            reference_value,
            MirRuntimeValue::Record {
                nominal: crate::core::ir::NominalTypeId::new("type:Point").expect("Point nominal"),
                fields: vec![MirRuntimeValue::Int(42), MirRuntimeValue::Bool(true)],
            }
        );

        let results = verify_program(&program, "record-source-hash".into()).expect("verify MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("record contract verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        assert!(program
            .functions()
            .get(&owner)
            .expect("record function")
            .canonical_text()
            .contains("project("));
    }

    #[test]
    fn verifier_and_reference_oracle_preserve_copy_record_update() {
        let source = r#"
            type Point { x: i32, enabled: bool }

            func advance(p: Point, next_x: i32) -> Point {
                requires: next_x >= 0 && next_x <= 100
                ensures: result.x == next_x && result.enabled == old(p.enabled)
                let updated = Point { x: next_x, ..p };
                updated
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("advance"))
            .cloned()
            .expect("advance MIR function");
        let point = crate::core::ir::NominalTypeId::new("type:Point").expect("Point nominal");

        let reference_value = MirReferenceInterpreter::new(&program)
            .execute(
                &owner,
                &[
                    MirRuntimeValue::Record {
                        nominal: point,
                        fields: vec![MirRuntimeValue::Int(7), MirRuntimeValue::Bool(true)],
                    },
                    MirRuntimeValue::Int(42),
                ],
            )
            .expect("reference record update execution");
        assert_eq!(
            reference_value,
            MirRuntimeValue::Record {
                nominal: crate::core::ir::NominalTypeId::new("type:Point").expect("Point nominal"),
                fields: vec![MirRuntimeValue::Int(42), MirRuntimeValue::Bool(true)],
            }
        );

        let results =
            verify_program(&program, "record-update-source-hash".into()).expect("verify MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("record update contract verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        assert!(program
            .functions()
            .get(&owner)
            .expect("record update function")
            .canonical_text()
            .contains("update_record"));
    }

    #[test]
    fn mir_gate_rejects_non_copy_record_update_without_transfer_contract() {
        let source = r#"
            type Box { text: string, count: i32 }

            func rewrite(value: Box, next_count: i32) -> Box {
                ensures: result.count == next_count
                let updated = Box { count: next_count, ..value };
                updated
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("non-Copy record contract must fail before verifier");
        assert!(matches!(
            error,
            crate::core::mir::reference::MirProgramBuildError::Validation(errors)
                if errors.iter().any(|error| error
                    .message
                    .contains("outside the canonical Copy aggregate contract"))
        ));
    }

    #[test]
    fn verifier_and_reference_oracle_preserve_copy_option_result_switch() {
        let source = r#"
            func classify_option(value: Option<i32>) -> i32 {
                ensures: result >= 0
                match value {
                    Some(v) => if v >= 0 { 1 } else { 0 },
                    None => 0
                }
            }

            func classify_result(value: Result<i32, i32>) -> i32 {
                ensures: result >= 0
                match value {
                    Ok(v) => if v >= 0 { 1 } else { 0 },
                    Err(e) => if e >= 0 { 1 } else { 0 }
                }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let option_owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("classify_option"))
            .cloned()
            .expect("Option classifier MIR function");
        let result_owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("classify_result"))
            .cloned()
            .expect("Result classifier MIR function");

        let option_nominal =
            crate::core::ir::NominalTypeId::new("builtin:type:Option").expect("Option nominal");
        let result_nominal =
            crate::core::ir::NominalTypeId::new("builtin:type:Result").expect("Result nominal");
        let option_some = crate::core::NodeId("builtin:variant:Option::Some".into());
        let result_err = crate::core::NodeId("builtin:variant:Result::Err".into());
        let option_value = MirReferenceInterpreter::new(&program)
            .execute(
                &option_owner,
                &[MirRuntimeValue::Variant {
                    nominal: option_nominal,
                    variant: option_some,
                    payload: vec![MirRuntimeValue::Int(41)],
                }],
            )
            .expect("reference Option execution");
        assert_eq!(option_value, MirRuntimeValue::Int(1));
        let result_value = MirReferenceInterpreter::new(&program)
            .execute(
                &result_owner,
                &[MirRuntimeValue::Variant {
                    nominal: result_nominal,
                    variant: result_err,
                    payload: vec![MirRuntimeValue::Int(-1)],
                }],
            )
            .expect("reference Result execution");
        assert_eq!(result_value, MirRuntimeValue::Int(0));

        let results = verify_program(&program, "variant-source-hash".into()).expect("verify MIR");
        for owner in [option_owner, result_owner] {
            let result = results
                .iter()
                .find(|result| result.func_name == owner.0)
                .expect("variant contract verification result");
            assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        }
    }

    #[test]
    fn verifier_rejects_non_copy_variant_switch_without_fallback() {
        let source = r#"
            func consume(value: Option<string>) -> i32 {
                ensures: result >= 0
                match value {
                    Some(_) => 1,
                    None => 0
                }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR gate");
        let results = verify_program(&program, "non-copy-variant-source-hash".into())
            .expect("verifier should return a classified result");
        let result = results
            .iter()
            .find(|result| result.func_name.ends_with("consume"))
            .expect("consume verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(result
            .message
            .contains("outside the Copy/no-op aggregate contract"));
    }

    #[test]
    fn verifier_preserves_checked_trap_class_through_copy_variant_switch() {
        let source = r#"
            func overflow(value: Option<i32>) -> i32 {
                ensures: result >= 0
                match value {
                    Some(v) => v + 2147483647,
                    None => 0
                }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let results =
            verify_program(&program, "variant-trap-source-hash".into()).expect("verify MIR");
        let result = results
            .iter()
            .find(|result| result.func_name.ends_with("overflow"))
            .expect("overflow verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Disproven);
        assert!(result.message.contains("can reach trap 'E0802'"));
    }
}
