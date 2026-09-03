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
use crate::core::mir::types::{
    MirAbiClass, MirBuiltinKind, MirGlueOperation, MirLayout, MirOwnership, MirTypeKind,
};
use crate::core::mir::{
    MirAggregateKind, MirContractBinaryOp, MirContractExpr, MirContractKind, MirContractUnaryOp,
    MirFunction, MirInstructionKind, MirListOperation, MirProjection, MirSwitchCase, MirTerminator,
    MirValueId,
};
use crate::verifier::ctx::{
    ProofArtifact, SolverSession, TrustedSubsetDomain, VerifStatus, VerificationResult,
};
use z3::ast::{Bool, Int, Set as Z3Set};
use z3::SatResult;
use z3::Sort;

#[derive(Debug, Clone)]
enum SymbolicValue {
    Int(Int),
    Bool(Bool),
    Unit,
    /// An owned value whose payload is intentionally opaque to the arithmetic
    /// contract domain.  The exact TypeDesc identity is retained so the
    /// verifier can still prove that Clone/Move/Drop operate on the same
    /// canonical ABI and ownership contract without inventing string
    /// semantics in Z3.
    Opaque {
        ty: crate::core::ResolvedTypeId,
    },
    Tuple(Vec<SymbolicValue>),
    Record {
        nominal: crate::core::ir::NominalTypeId,
        fields: BTreeMap<crate::core::NodeId, SymbolicValue>,
    },
    /// A scalar Set is modeled as a Z3 set plus an explicitly tracked size.
    /// The size is part of the MIR/runtime contract because Z3's Set sort has
    /// no cardinality operator.  Insert/remove update it through the member
    /// predicate, while the non-negative invariant is carried as a solver
    /// constraint.
    Set {
        elements: Z3Set,
        size: Int,
    },
    /// A scalar canonical List is represented by its length in the verifier.
    /// Set-to-list does not expose HashSet iteration order as a proof
    /// obligation; the runtime order is fixed by the MIR contract.
    List {
        length: Int,
    },
    /// A symbolic built-in Option/Result value.  The tag is constrained to
    /// the canonical TypeDesc discriminants when the value is introduced;
    /// payloads are keyed by stable field identity so switch bindings never
    /// infer a payload slot from source-pattern position.
    Variant {
        nominal: crate::core::ir::NominalTypeId,
        tag: Int,
        payload: BTreeMap<crate::core::NodeId, SymbolicValue>,
        /// `None` means this is a symbolic input whose payload contains the
        /// union of all variant fields. `Some` records the active shape of a
        /// canonical construction, so result-shape checks do not confuse an
        /// active zero-field `None`/`Err` with the input union shape.
        active_variant: Option<crate::core::NodeId>,
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
    program.canonical_digest()
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

    // The verifier admits an owned String result only through the same
    // canonical one-block Move/Clone/Drop ledger used by MIR construction and
    // native admission. String payloads stay opaque in Z3, but their TypeDesc
    // ABI and exactly-once ownership transfer are still checked before any
    // arithmetic contract is proved.
    if crate::core::mir::is_owned_string_return_candidate(function, catalog) {
        crate::core::mir::validate_owned_string_return_shape(function, catalog)
            .map_err(|message| format!("canonical MIR owned String return rejected: {message}"))?;
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
        program,
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
    if descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String) {
        catalog.validate_owned_string(ty)?;
        return Ok((SymbolicValue::Opaque { ty: ty.clone() }, Vec::new()));
    }
    if descriptor.kind == MirTypeKind::Set {
        catalog.validate_set_glue(ty, MirGlueOperation::MoveOut)?;
        let MirLayout::Set { element } = &descriptor.layout else {
            return Err(format!(
                "MIR verifier Set TypeDesc '{}' has no canonical Set<T> layout",
                ty.as_str()
            ));
        };
        let sort = set_element_sort(catalog, element)?;
        let elements = Z3Set::new_const(format!("{name}.elements"), &sort);
        let size = Int::new_const(format!("{name}.size"));
        return Ok((
            SymbolicValue::Set {
                elements,
                size: size.clone(),
            },
            vec![size.ge(Int::from_i64(0))],
        ));
    }
    if descriptor.kind == MirTypeKind::List {
        catalog.validate_list_glue(ty, MirGlueOperation::MoveOut)?;
        let length = Int::new_const(format!("{name}.length"));
        return Ok((
            SymbolicValue::List {
                length: length.clone(),
            },
            vec![length.ge(Int::from_i64(0))],
        ));
    }
    let non_copy_record = matches!(
        descriptor.ownership,
        MirOwnership::Move | MirOwnership::Linear
    ) && matches!(&descriptor.layout, MirLayout::Record { .. })
        && descriptor.abi == MirAbiClass::Aggregate
        && descriptor.glue.move_out == crate::core::mir::types::MirGlueKind::Aggregate
        && descriptor.glue.clone == crate::core::mir::types::MirGlueKind::Aggregate
        && descriptor.glue.drop == crate::core::mir::types::MirGlueKind::Aggregate
        && descriptor.drop_plan.is_some();
    let move_owned_variant = catalog.validate_non_copy_variant_contract(ty).is_ok();
    let move_owned_tuple = matches!(descriptor.layout, MirLayout::Tuple(_))
        && catalog.validate_recursive_tuple_abi(ty).is_ok();
    if (descriptor.ownership != MirOwnership::Copy
        && !non_copy_record
        && !move_owned_variant
        && !move_owned_tuple)
        || (!non_copy_record
            && !move_owned_variant
            && !move_owned_tuple
            && (descriptor.glue.move_out != crate::core::mir::types::MirGlueKind::Noop
                || descriptor.glue.clone != crate::core::mir::types::MirGlueKind::Noop
                || descriptor.glue.drop != crate::core::mir::types::MirGlueKind::Noop))
    {
        return Err(format!(
            "MIR verifier TypeDesc '{}' is outside the Copy/no-op aggregate contract",
            ty.as_str()
        ));
    }
    match &descriptor.layout {
        MirLayout::Unit if descriptor.abi == MirAbiClass::Unit => {
            Ok((SymbolicValue::Unit, Vec::new()))
        }
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
                    active_variant: None,
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

fn set_element_sort(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    element: &crate::core::ir::ResolvedTypeId,
) -> Result<Sort, String> {
    let descriptor = catalog.get(element).ok_or_else(|| {
        format!(
            "MIR verifier Set element TypeDesc '{}' is absent",
            element.as_str()
        )
    })?;
    if descriptor.ownership != MirOwnership::Copy
        || descriptor.glue.move_out != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.glue.clone != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.glue.drop != crate::core::mir::types::MirGlueKind::Noop
        || descriptor.layout != MirLayout::Scalar
    {
        return Err(format!(
            "MIR verifier Set element '{}' is outside the Copy scalar contract",
            element.as_str()
        ));
    }
    match descriptor.abi {
        MirAbiClass::Integer {
            bits: 32 | 64,
            signed: true,
        } => Ok(Sort::int()),
        MirAbiClass::Bool => Ok(Sort::bool()),
        abi => Err(format!(
            "MIR verifier Set element ABI {:?} is outside the checked scalar contract",
            abi
        )),
    }
}

fn set_member(elements: &Z3Set, value: &SymbolicValue) -> Result<Bool, String> {
    match value {
        SymbolicValue::Int(value) => Ok(elements.member(value)),
        SymbolicValue::Bool(value) => Ok(elements.member(value)),
        _ => Err("MIR verifier Set operation requires a scalar element".into()),
    }
}

fn set_add(elements: &Z3Set, value: &SymbolicValue) -> Result<Z3Set, String> {
    match value {
        SymbolicValue::Int(value) => Ok(elements.add(value)),
        SymbolicValue::Bool(value) => Ok(elements.add(value)),
        _ => Err("MIR verifier Set construction requires a scalar element".into()),
    }
}

fn set_del(elements: &Z3Set, value: &SymbolicValue) -> Result<Z3Set, String> {
    match value {
        SymbolicValue::Int(value) => Ok(elements.del(value)),
        SymbolicValue::Bool(value) => Ok(elements.del(value)),
        _ => Err("MIR verifier Set operation requires a scalar element".into()),
    }
}

fn symbolic_set_operation(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result_ty: &crate::core::ResolvedTypeId,
    set_ty: &crate::core::ResolvedTypeId,
    operation: crate::core::mir::MirSetOperation,
    receiver: SymbolicValue,
    argument: Option<SymbolicValue>,
) -> Result<SymbolicValue, String> {
    let SymbolicValue::Set { elements, size } = receiver else {
        return Err("MIR verifier Set operation receiver is not a symbolic Set".into());
    };
    let output = match operation {
        crate::core::mir::MirSetOperation::Size => SymbolicValue::Int(size),
        crate::core::mir::MirSetOperation::IsEmpty => {
            SymbolicValue::Bool(size.eq(Int::from_i64(0)))
        }
        crate::core::mir::MirSetOperation::Contains => {
            let value = argument
                .as_ref()
                .ok_or_else(|| "MIR verifier Set.contains argument is absent".to_string())?;
            SymbolicValue::Bool(set_member(&elements, value)?)
        }
        crate::core::mir::MirSetOperation::Insert | crate::core::mir::MirSetOperation::Remove => {
            let value = argument
                .as_ref()
                .ok_or_else(|| "MIR verifier Set mutation argument is absent".to_string())?;
            let present = set_member(&elements, value)?;
            let is_insert = operation == crate::core::mir::MirSetOperation::Insert;
            let delta = if is_insert {
                present.ite(&Int::from_i64(0), &Int::from_i64(1))
            } else {
                state
                    .constraints
                    .push(present.implies(size.ge(Int::from_i64(1))));
                present.ite(&Int::from_i64(1), &Int::from_i64(0))
            };
            let next_size = if is_insert {
                Int::add(&[&size, &delta])
            } else {
                Int::sub(&[&size, &delta])
            };
            let next_elements = if is_insert {
                set_add(&elements, value)?
            } else {
                set_del(&elements, value)?
            };
            SymbolicValue::Set {
                elements: next_elements,
                size: next_size,
            }
        }
        crate::core::mir::MirSetOperation::ToList => {
            let list_desc = catalog
                .get(result_ty)
                .ok_or_else(|| "MIR verifier Set.to_list result TypeDesc is absent".to_string())?;
            let MirLayout::List { element } = &list_desc.layout else {
                return Err("MIR verifier Set.to_list result has no List layout".into());
            };
            let set_desc = catalog.get(set_ty).ok_or_else(|| {
                "MIR verifier Set.to_list receiver TypeDesc is absent".to_string()
            })?;
            let MirLayout::Set {
                element: set_element,
            } = &set_desc.layout
            else {
                return Err("MIR verifier Set.to_list receiver has no Set layout".into());
            };
            if element != set_element {
                return Err("MIR verifier Set.to_list element types disagree".into());
            }
            SymbolicValue::List { length: size }
        }
    };
    Ok(output)
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
    field_types: &[crate::core::ir::ResolvedTypeId],
) -> Result<SymbolicValue, String> {
    let field_ids = fields
        .iter()
        .map(|(field_id, _)| field_id.clone())
        .collect::<Vec<_>>();
    let expected_variant = catalog.validated_variant_construct(
        result_ty,
        nominal,
        variant,
        &field_ids,
        field_types,
    )?;
    let mut payload = BTreeMap::new();
    for ((field_id, value), field_ty) in fields.iter().zip(field_types) {
        if !symbolic_matches_type(catalog, field_ty, value) {
            return Err("MIR verifier variant payload disagrees with TypeDesc".into());
        }
        if payload.insert(field_id.clone(), value.clone()).is_some() {
            return Err("MIR verifier variant construction repeats a field".into());
        }
    }
    Ok(SymbolicValue::Variant {
        nominal: nominal.clone(),
        tag: Int::from_i64(expected_variant.discriminant as i64),
        payload,
        active_variant: Some(expected_variant.id.clone()),
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
    program: &MirProgram,
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
        eval_instruction(function, program, catalog, state, &instruction.kind)?;
    }
    match &block.terminator {
        MirTerminator::Goto {
            target, arguments, ..
        } => {
            let mut next = edge_state(state, function, target, arguments)?;
            explore_block(function, program, catalog, &mut next, target, active, returns, traps)?;
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
                program,
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
                program,
                catalog,
                &mut else_state,
                else_target,
                &mut active.clone(),
                returns,
                traps,
            )?;
        }
        MirTerminator::Switch { scrutinee, arms } => explore_variant_switch(
            function,
            program,
            catalog,
            state,
            scrutinee,
            arms,
            false,
            active,
            returns,
            traps,
        )?,
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
        MirTerminator::SwitchMove { scrutinee, arms } => explore_variant_switch(
            function,
            program,
            catalog,
            state,
            scrutinee,
            arms,
            true,
            active,
            returns,
            traps,
        )?,
        MirTerminator::Fault { .. } => {
            return Err(
                "canonical MIR verifier currently supports scalar Goto/Branch and canonical variant Switch CFG".into(),
            )
        }
    }
    active.remove(block_id);
    Ok(())
}

fn explore_variant_switch(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    scrutinee: &MirValueId,
    arms: &[crate::core::mir::MirSwitchArm],
    consume_scrutinee: bool,
    active: &mut BTreeSet<crate::core::mir::MirBlockId>,
    returns: &mut Vec<ReturnPath>,
    traps: &mut Vec<SymbolicTrap>,
) -> Result<(), String> {
    let scrutinee_ty = function
        .values
        .get(scrutinee)
        .map(|value| value.ty.clone())
        .ok_or_else(|| format!("switch scrutinee '{}' has no TypeDesc", scrutinee))?;
    if consume_scrutinee {
        catalog.validate_non_copy_variant_contract(&scrutinee_ty)?;
        catalog.validate_variant_switch_move_contract(&scrutinee_ty, arms)?;
        validate_explicit_variant_switch_move(catalog, &scrutinee_ty, arms)?;
    } else {
        catalog.validate_switch(&scrutinee_ty, arms)?;
    }
    let value = if consume_scrutinee {
        state
            .values
            .remove(scrutinee)
            .ok_or_else(|| format!("switch-move scrutinee '{}' is not defined", scrutinee))?
    } else {
        state
            .values
            .get(scrutinee)
            .cloned()
            .ok_or_else(|| format!("switch scrutinee '{}' is not defined", scrutinee))?
    };
    let SymbolicValue::Variant {
        nominal,
        tag,
        payload,
        active_variant,
    } = value
    else {
        return Err(
            "canonical MIR verifier variant switch requires a symbolic Option/Result value".into(),
        );
    };
    let Some((expected_nominal, variants)) = catalog.variant_layout(&scrutinee_ty) else {
        return Err(
            "canonical MIR verifier variant switch has no canonical TypeDesc layout".into(),
        );
    };
    if nominal.as_str() != expected_nominal {
        return Err("canonical MIR verifier variant switch nominal disagrees with TypeDesc".into());
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
                    let parameter = function.values.get(&binding.parameter).ok_or_else(|| {
                        format!(
                            "canonical MIR verifier switch binding parameter '{}' is absent",
                            binding.parameter
                        )
                    })?;
                    catalog.validate_variant_payload_projection_receipt(
                        &scrutinee_ty,
                        variant_id,
                        &parameter.ty,
                        &binding.projection,
                    )?;
                    let field_index = binding.projection.field_index;
                    let field = variant.fields.get(field_index).ok_or_else(|| {
                        format!(
                            "canonical MIR verifier switch payload field '{}' is absent",
                            binding.projection.field.0
                        )
                    })?;
                    // A direct move-owned call returns one known active variant. The
                    // verifier still walks the exhaustive inactive arm to prove its
                    // contract, so give only that unreachable binding a typed opaque /
                    // zero placeholder; an active arm remains strict about payload
                    // presence and type.
                    let value = if let Some(active_variant) = active_variant.as_ref() {
                        if active_variant != variant_id {
                            symbolic_zero_for_type(catalog, &field.ty)?
                        } else {
                            payload.get(&field.id).cloned().ok_or_else(|| {
                                format!(
                                    "canonical MIR verifier switch payload field '{}' is absent",
                                    field.id.0
                                )
                            })?
                        }
                    } else {
                        payload.get(&field.id).cloned().ok_or_else(|| {
                            format!(
                                "canonical MIR verifier switch payload field '{}' is absent",
                                field.id.0
                            )
                        })?
                    };
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
            MirSwitchCase::Default => (symbolic_default_guard(&previous_cases), Vec::new()),
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
            program,
            catalog,
            &mut next,
            &arm.target,
            &mut active.clone(),
            returns,
            traps,
        )?;
    }
    Ok(())
}

/// The verifier's admitted move-variant island has no symbolic encoding for a
/// default arm: every canonical TypeDesc variant must be explored explicitly
/// so the active payload/drop proof is visible in the MIR CFG.  Keep this
/// defense at the symbolic consumer boundary as well as in the capability
/// gate; callers that invoke the MIR engine directly must remain fail-closed.
fn validate_explicit_variant_switch_move(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    scrutinee_ty: &crate::core::ResolvedTypeId,
    arms: &[crate::core::mir::MirSwitchArm],
) -> Result<(), String> {
    let Some((_, variants)) = catalog.variant_layout(scrutinee_ty) else {
        return Err("canonical MIR verifier SwitchMove has no variant layout".into());
    };
    if arms.len() != variants.len() {
        return Err(
            "canonical MIR verifier SwitchMove requires exactly one explicit arm for each TypeDesc variant".into(),
        );
    }
    let required = variants
        .iter()
        .map(|variant| variant.id.clone())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for arm in arms {
        let MirSwitchCase::Variant(variant_id) = &arm.case else {
            return Err(
                "canonical MIR verifier SwitchMove requires explicit variant arms; default/literal cases are not covered".into(),
            );
        };
        if !seen.insert(variant_id.clone()) {
            return Err(format!(
                "canonical MIR verifier SwitchMove variant '{}' is repeated",
                variant_id.0
            ));
        }
    }
    if seen != required {
        return Err(
            "canonical MIR verifier SwitchMove does not cover exactly the TypeDesc variants".into(),
        );
    }
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
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    instruction: &MirInstructionKind,
) -> Result<(), String> {
    match instruction {
        MirInstructionKind::Const { result, literal } => {
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR const result '{}' is absent", result))?
                .ty
                .clone();
            let value = match literal {
                crate::core::ir::ResolvedLiteral::Int(value) => {
                    let ScalarKind::Int { .. } = value_scalar_kind(function, catalog, result)?
                    else {
                        return Err("MIR scalar const literal disagrees with TypeDesc ABI".into());
                    };
                    SymbolicValue::Int(Int::from_i64(*value))
                }
                crate::core::ir::ResolvedLiteral::Bool(value) => {
                    let ScalarKind::Bool = value_scalar_kind(function, catalog, result)? else {
                        return Err("MIR scalar const literal disagrees with TypeDesc ABI".into());
                    };
                    SymbolicValue::Bool(Bool::from_bool(*value))
                }
                crate::core::ir::ResolvedLiteral::String(_) => {
                    catalog.validate_owned_string(&result_ty)?;
                    SymbolicValue::Opaque { ty: result_ty }
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
        MirInstructionKind::Copy { result, source } => {
            let source_ty = instruction_value_type(function, source, "copy source")?;
            ensure_copy_value(function, catalog, source)?;
            ensure_same_instruction_types(function, result, &source_ty, "copy")?;
            let value = state
                .values
                .get(source)
                .cloned()
                .ok_or_else(|| format!("MIR value '{}' is not defined", source))?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Move { result, source } => {
            let source_ty = instruction_value_type(function, source, "move source")?;
            ensure_same_instruction_types(function, result, &source_ty, "move")?;
            let is_copy = catalog
                .get(&source_ty)
                .is_some_and(|descriptor| descriptor.ownership == MirOwnership::Copy);
            let value = if is_copy {
                ensure_copy_value(function, catalog, source)?;
                state
                    .values
                    .get(source)
                    .cloned()
                    .ok_or_else(|| format!("MIR value '{}' is not defined", source))?
            } else {
                catalog.validate_glue(&source_ty, MirGlueOperation::MoveOut)?;
                state
                    .values
                    .remove(source)
                    .ok_or_else(|| format!("MIR value '{}' is not available for move", source))?
            };
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Clone { result, source } => {
            let source_ty = instruction_value_type(function, source, "clone source")?;
            ensure_same_instruction_types(function, result, &source_ty, "clone")?;
            let is_copy = catalog
                .get(&source_ty)
                .is_some_and(|descriptor| descriptor.ownership == MirOwnership::Copy);
            if is_copy {
                ensure_copy_value(function, catalog, source)?;
            } else {
                catalog.validate_glue(&source_ty, MirGlueOperation::Clone)?;
            }
            let value = state
                .values
                .get(source)
                .cloned()
                .ok_or_else(|| format!("MIR value '{}' is not defined", source))?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Drop { value } => {
            let ty = instruction_value_type(function, value, "drop value")?;
            let is_copy = catalog
                .get(&ty)
                .is_some_and(|descriptor| descriptor.ownership == MirOwnership::Copy);
            if is_copy {
                ensure_copy_value(function, catalog, value)?;
            } else {
                catalog.validate_glue(&ty, MirGlueOperation::Drop)?;
            }
            if !is_copy {
                let dropped = state
                    .values
                    .remove(value)
                    .ok_or_else(|| format!("MIR drop value '{}' is not defined", value))?;
                if !symbolic_matches_type(catalog, &ty, &dropped) {
                    return Err(format!(
                        "MIR drop value '{}' disagrees with TypeDesc",
                        value
                    ));
                }
            } else if !state.values.contains_key(value) {
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
                (MirBuiltinKind::PrintlnBool, [SymbolicValue::Bool(_)]) => SymbolicValue::Unit,
                (MirBuiltinKind::PrintlnInt, [SymbolicValue::Int(_)]) => SymbolicValue::Unit,
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
            list_index_contract,
        } => {
            let value = state
                .values
                .get(base)
                .cloned()
                .ok_or_else(|| format!("MIR projection base '{}' is not defined", base))?;
            let value = if let MirProjection::Index(index) = projection {
                let base_ty = function
                    .values
                    .get(base)
                    .ok_or_else(|| format!("MIR List projection base '{}' is absent", base))?
                    .ty
                    .clone();
                let result_ty = function
                    .values
                    .get(result)
                    .ok_or_else(|| format!("MIR List projection result '{}' is absent", result))?
                    .ty
                    .clone();
                let index_ty = function
                    .values
                    .get(index)
                    .ok_or_else(|| format!("MIR List index '{}' is absent", index))?
                    .ty
                    .clone();
                let receipt = list_index_contract.as_ref().ok_or_else(|| {
                    "MIR List index projection has no canonical receipt".to_string()
                })?;
                catalog.validate_list_index_projection_receipt(
                    &base_ty, &index_ty, &result_ty, receipt,
                )?;
                let SymbolicValue::List { length } =
                    state.values.get(base).cloned().ok_or_else(|| {
                        format!("MIR List projection base '{}' is not defined", base)
                    })?
                else {
                    return Err("MIR List projection base is not a symbolic List".into());
                };
                let index_value = state
                    .values
                    .get(index)
                    .cloned()
                    .ok_or_else(|| format!("MIR List index '{}' is not defined", index))?;
                let raw = match index_value {
                    SymbolicValue::Int(raw) => raw,
                    _ => return Err("MIR List index is not a symbolic signed integer".into()),
                };
                let zero = Int::from_i64(0);
                let length_as_int = length.clone();
                let non_negative = raw.ge(&zero);
                let forward = Bool::and(&[&non_negative, &raw.lt(&length_as_int)]);
                let negative = raw.lt(&zero);
                let backward = Bool::and(&[&negative, &raw.ge(&length_as_int.unary_minus())]);
                let in_bounds = Bool::or(&[&forward, &backward]);
                add_definedness(state, in_bounds, "E0803")?;
                let (projected, constraints) = symbolic_value_for_type(
                    catalog,
                    &result_ty,
                    &format!("mir.project.{}", result),
                )?;
                state.constraints.extend(constraints);
                projected
            } else if matches!(projection, MirProjection::Dereference) {
                let base_ty = function
                    .values
                    .get(base)
                    .ok_or_else(|| format!("MIR dereference base '{}' is absent", base))?
                    .ty
                    .clone();
                let result_ty = function
                    .values
                    .get(result)
                    .ok_or_else(|| format!("MIR dereference result '{}' is absent", result))?
                    .ty
                    .clone();
                let target = catalog.validate_reference_type(&base_ty)?;
                catalog.validate_dereference(&base_ty, &result_ty)?;
                if !symbolic_matches_type(catalog, &target, &value) {
                    return Err(
                        "MIR dereference value disagrees with its canonical reference target"
                            .into(),
                    );
                }
                value
            } else {
                if list_index_contract.is_some() {
                    return Err(
                        "MIR List index receipt is attached to a non-index projection".into(),
                    );
                }
                symbolic_project(value, projection)?
            };
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::VariantProject {
            result,
            base,
            contract,
        } => {
            let base_ty = function
                .values
                .get(base)
                .ok_or_else(|| format!("MIR variant projection base '{}' is absent", base))?
                .ty
                .clone();
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR variant projection result '{}' is absent", result))?
                .ty
                .clone();
            let receipt = contract.as_ref().ok_or_else(|| {
                "MIR direct variant projection has no canonical trap receipt".to_string()
            })?;
            catalog.validate_variant_projection_trap_receipt(&base_ty, &result_ty, receipt)?;
            let value =
                state.values.get(base).cloned().ok_or_else(|| {
                    format!("MIR variant projection base '{}' is not defined", base)
                })?;
            let SymbolicValue::Variant {
                nominal,
                tag,
                payload,
                active_variant,
            } = value
            else {
                return Err(
                    "MIR direct variant projection requires a symbolic Option/Result value".into(),
                );
            };
            if nominal != receipt.projection.nominal {
                return Err("MIR direct variant projection nominal disagrees with TypeDesc".into());
            }
            let active = tag.eq(Int::from_i64(receipt.discriminant as i64));
            add_definedness(state, active, &receipt.trap_code)?;
            let projected = if active_variant
                .as_ref()
                .is_some_and(|variant| variant != &receipt.projection.variant)
            {
                symbolic_zero_for_type(catalog, &result_ty)?
            } else {
                payload
                    .get(&receipt.projection.field)
                    .cloned()
                    .ok_or_else(|| {
                        "MIR direct variant projection payload field is absent".to_string()
                    })?
            };
            ensure_result_shape(function, catalog, result, &projected)?;
            state.values.insert(result.clone(), projected);
        }
        MirInstructionKind::VariantProjectMove {
            result,
            base,
            contract,
        } => {
            let base_ty = function
                .values
                .get(base)
                .ok_or_else(|| format!("MIR variant move projection base '{}' is absent", base))?
                .ty
                .clone();
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| {
                    format!("MIR variant move projection result '{}' is absent", result)
                })?
                .ty
                .clone();
            let receipt = contract.as_ref().ok_or_else(|| {
                "MIR consuming direct variant projection has no canonical move receipt".to_string()
            })?;
            catalog.validate_variant_move_projection_trap_receipt(&base_ty, &result_ty, receipt)?;
            let value = state.values.remove(base).ok_or_else(|| {
                format!("MIR variant move projection base '{}' is not defined", base)
            })?;
            let SymbolicValue::Variant {
                nominal,
                tag,
                payload,
                active_variant,
            } = value
            else {
                return Err(
                    "MIR consuming direct variant projection requires a symbolic Option/Result value"
                        .into(),
                );
            };
            if nominal != receipt.projection.nominal {
                return Err(
                    "MIR consuming direct variant projection nominal disagrees with TypeDesc"
                        .into(),
                );
            }
            let active = tag.eq(Int::from_i64(receipt.discriminant as i64));
            add_definedness(state, active, &receipt.trap_code)?;
            let projected = if active_variant
                .as_ref()
                .is_some_and(|variant| variant != &receipt.projection.variant)
            {
                symbolic_zero_for_type(catalog, &result_ty)?
            } else {
                payload
                    .get(&receipt.projection.field)
                    .cloned()
                    .ok_or_else(|| {
                        "MIR consuming direct variant projection payload field is absent"
                            .to_string()
                    })?
            };
            ensure_result_shape(function, catalog, result, &projected)?;
            state.values.insert(result.clone(), projected);
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
        MirInstructionKind::ConstructList {
            result,
            elements,
            list_construct_contract,
        } => {
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR List result '{}' is absent", result))?
                .ty
                .clone();
            let element_types = elements
                .iter()
                .map(|value| {
                    function
                        .values
                        .get(value)
                        .map(|info| info.ty.clone())
                        .ok_or_else(|| format!("MIR List element '{}' is absent", value))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let receipt = list_construct_contract.as_ref().ok_or_else(|| {
                "MIR verifier List construction has no canonical receipt".to_string()
            })?;
            catalog.validate_list_construct_receipt(&result_ty, &element_types, receipt)?;
            for value in elements {
                if !state.values.contains_key(value) {
                    return Err(format!("MIR List element '{}' is not defined", value));
                }
            }
            let value = SymbolicValue::List {
                length: Int::from_i64(elements.len() as i64),
            };
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::ConstructVariant {
            result,
            nominal,
            variant,
            fields,
        } => {
            let field_types = fields
                .iter()
                .map(|(_, value)| {
                    function
                        .values
                        .get(value)
                        .map(|info| info.ty.clone())
                        .ok_or_else(|| format!("MIR variant payload value '{}' is absent", value))
                })
                .collect::<Result<Vec<_>, _>>()?;
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
                &field_types,
            )?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::ListOp {
            result,
            operation,
            list,
            argument,
            list_operation_contract,
        } => {
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR List operation result '{}' is absent", result))?
                .ty
                .clone();
            let list_ty = function
                .values
                .get(list)
                .ok_or_else(|| format!("MIR List operation receiver '{}' is absent", list))?
                .ty
                .clone();
            let argument_ty = argument
                .as_ref()
                .map(|value| {
                    function
                        .values
                        .get(value)
                        .map(|info| info.ty.clone())
                        .ok_or_else(|| format!("MIR List operation argument '{}' is absent", value))
                })
                .transpose()?;
            let receipt = list_operation_contract
                .as_ref()
                .ok_or_else(|| "MIR List operation has no canonical receipt".to_string())?;
            catalog.validate_list_operation_receipt_with_argument(
                &result_ty,
                &list_ty,
                argument_ty.as_ref(),
                *operation,
                receipt,
            )?;
            let value = match operation {
                MirListOperation::Len => {
                    let SymbolicValue::List { length } =
                        state.values.get(list).cloned().ok_or_else(|| {
                            format!("MIR List receiver '{}' is not defined", list)
                        })?
                    else {
                        return Err("MIR List operation receiver is not a symbolic List".into());
                    };
                    let fits_i32 = length.le(Int::from_i64(i32::MAX as i64));
                    add_definedness(state, fits_i32, "E0802")?;
                    SymbolicValue::Int(length)
                }
                // Reverse clones the scalar List and therefore preserves its
                // symbolic cardinality while leaving the source value live.
                MirListOperation::Reverse => {
                    let SymbolicValue::List { length } =
                        state.values.get(list).cloned().ok_or_else(|| {
                            format!("MIR List receiver '{}' is not defined", list)
                        })?
                    else {
                        return Err("MIR List operation receiver is not a symbolic List".into());
                    };
                    SymbolicValue::List { length }
                }
                MirListOperation::Concat => {
                    let Some(argument) = argument else {
                        return Err("MIR List.concat operation has no second input".into());
                    };
                    let SymbolicValue::List { length: left } = state
                        .values
                        .remove(list)
                        .ok_or_else(|| format!("MIR List receiver '{}' is not defined", list))?
                    else {
                        return Err("MIR List.concat receiver is not a symbolic List".into());
                    };
                    let SymbolicValue::List { length: right } =
                        state.values.remove(argument).ok_or_else(|| {
                            format!("MIR List argument '{}' is not defined", argument)
                        })?
                    else {
                        return Err("MIR List.concat argument is not a symbolic List".into());
                    };
                    let length = Int::add(&[&left, &right]);
                    add_definedness(state, length.le(Int::from_i64(i64::MAX)), "E0800")?;
                    SymbolicValue::List { length }
                }
            };
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::ConstructSet { result, elements } => {
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR Set result '{}' is absent", result))?
                .ty
                .clone();
            let element_types = elements
                .iter()
                .map(|value| {
                    function
                        .values
                        .get(value)
                        .map(|info| info.ty.clone())
                        .ok_or_else(|| format!("MIR Set element '{}' is absent", value))
                })
                .collect::<Result<Vec<_>, _>>()?;
            catalog.validate_set_construct(&result_ty, &element_types)?;
            let element_ty = match &catalog
                .get(&result_ty)
                .ok_or_else(|| format!("MIR Set result '{}' TypeDesc is absent", result))?
                .layout
            {
                MirLayout::Set { element } => element.clone(),
                _ => return Err("MIR Set construction has no canonical Set layout".into()),
            };
            let sort = set_element_sort(catalog, &element_ty)?;
            let mut set = Z3Set::empty(&sort);
            let mut size = Int::from_i64(0);
            for value in elements {
                let value = state
                    .values
                    .get(value)
                    .cloned()
                    .ok_or_else(|| format!("MIR Set element '{}' is not defined", value))?;
                let present = set_member(&set, &value)?;
                let increment = present.ite(&Int::from_i64(0), &Int::from_i64(1));
                size = Int::add(&[&size, &increment]);
                set = set_add(&set, &value)?;
            }
            let value = SymbolicValue::Set {
                elements: set,
                size,
            };
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::SetOp {
            result,
            operation,
            set,
            argument,
        } => {
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR Set operation result '{}' is absent", result))?
                .ty
                .clone();
            let set_ty = function
                .values
                .get(set)
                .ok_or_else(|| format!("MIR Set operation receiver '{}' is absent", set))?
                .ty
                .clone();
            let argument_ty = argument
                .as_ref()
                .map(|value| {
                    function
                        .values
                        .get(value)
                        .map(|info| info.ty.clone())
                        .ok_or_else(|| format!("MIR Set operation argument '{}' is absent", value))
                })
                .transpose()?;
            catalog.validate_set_operation(
                &result_ty,
                &set_ty,
                argument_ty.as_ref(),
                *operation,
            )?;
            let SymbolicValue::Set { elements, size } = state
                .values
                .get(set)
                .cloned()
                .ok_or_else(|| format!("MIR Set receiver '{}' is not defined", set))?
            else {
                return Err("MIR Set operation receiver is not a symbolic Set".into());
            };
            let argument_value = argument
                .as_ref()
                .map(|argument| {
                    state.values.get(argument).cloned().ok_or_else(|| {
                        format!("MIR Set operation argument '{}' is not defined", argument)
                    })
                })
                .transpose()?;
            let output = symbolic_set_operation(
                catalog,
                state,
                &result_ty,
                &set_ty,
                *operation,
                SymbolicValue::Set { elements, size },
                argument_value,
            )?;
            ensure_result_shape(function, catalog, result, &output)?;
            state.values.insert(result.clone(), output);
        }
        MirInstructionKind::MoveProject {
            result,
            base,
            projection,
        } => {
            let base_ty = instruction_value_type(function, base, "move projection base")?;
            let result_ty = instruction_value_type(function, result, "move projection result")?;
            catalog.validate_move_projection(&base_ty, &result_ty, projection)?;
            let MirProjection::Field(field) = projection else {
                return Err("MIR move projection requires a direct record field".into());
            };
            let receipt =
                catalog.validated_record_field_projection_contract(&base_ty, field, &result_ty)?;
            let base_value = state
                .values
                .remove(base)
                .ok_or_else(|| format!("MIR move projection base '{}' is not defined", base))?;
            if !symbolic_matches_type(catalog, &base_ty, &base_value) {
                return Err("MIR move projection base disagrees with TypeDesc".into());
            }
            let projected = symbolic_project(base_value, &MirProjection::Field(receipt.field))?;
            ensure_result_shape(function, catalog, result, &projected)?;
            state.values.insert(result.clone(), projected);
        }
        MirInstructionKind::MoveProjectDrop {
            result,
            base,
            projection,
            contract,
        } => {
            let base_ty = instruction_value_type(function, base, "move/drop projection base")?;
            let result_ty =
                instruction_value_type(function, result, "move/drop projection result")?;
            let MirProjection::Field(field) = projection else {
                return Err("MIR move/drop projection requires a direct record field".into());
            };
            let Some(receipt) = contract.as_ref() else {
                return Err("MIR move/drop projection has no canonical residual receipt".into());
            };
            catalog.validate_record_move_projection_drop_receipt(&base_ty, &result_ty, receipt)?;
            if receipt.projection.field != *field {
                return Err("MIR move/drop projection field disagrees with its receipt".into());
            }
            let base_value = state.values.remove(base).ok_or_else(|| {
                format!("MIR move/drop projection base '{}' is not defined", base)
            })?;
            let SymbolicValue::Record {
                nominal,
                mut fields,
            } = base_value
            else {
                return Err("MIR move/drop projection base is not a symbolic record".into());
            };
            if nominal != receipt.projection.nominal {
                return Err("MIR move/drop projection nominal disagrees with TypeDesc".into());
            }
            if fields.len() != receipt.projection.arity {
                return Err("MIR move/drop projection record arity disagrees with TypeDesc".into());
            }
            let projected = fields
                .remove(&receipt.projection.field)
                .ok_or_else(|| "MIR move/drop projection selected field is absent".to_string())?;
            for residual in &receipt.residual {
                fields.remove(&residual.id).ok_or_else(|| {
                    format!(
                        "MIR move/drop projection residual field '{}' is absent",
                        residual.name
                    )
                })?;
            }
            if !fields.is_empty() {
                return Err(
                    "MIR move/drop projection has fields outside its TypeDesc receipt".into(),
                );
            }
            ensure_result_shape(function, catalog, result, &projected)?;
            state.values.insert(result.clone(), projected);
        }
        MirInstructionKind::ConstructVariantMove {
            result,
            nominal,
            variant,
            fields,
        } => {
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR variant result '{}' is absent", result))?
                .ty
                .clone();
            catalog.validate_non_copy_variant_contract(&result_ty)?;
            let mut values = Vec::with_capacity(fields.len());
            let mut field_types = Vec::with_capacity(fields.len());
            for (field, value) in fields {
                let value_ty = function
                    .values
                    .get(value)
                    .ok_or_else(|| format!("MIR variant payload value '{}' is absent", value))?
                    .ty
                    .clone();
                field_types.push(value_ty);
                let symbolic = state.values.remove(value).ok_or_else(|| {
                    format!("MIR variant payload value '{}' is not defined", value)
                })?;
                values.push((field.clone(), symbolic));
            }
            let value = symbolic_variant_construct(
                catalog,
                &result_ty,
                nominal,
                variant,
                &values,
                &field_types,
            )?;
            ensure_result_shape(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Call {
            result,
            callee,
            type_arguments,
            arguments,
            variant_call_contract,
        } => eval_materialized_call(
            function,
            program,
            catalog,
            state,
            result,
            callee,
            type_arguments,
            arguments,
            variant_call_contract.as_ref(),
        )?,
        MirInstructionKind::VariantPredicate {
            result,
            predicate,
            variant,
            contract,
        } => {
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR variant predicate result '{}' is absent", result))?
                .ty
                .clone();
            let variant_ty = function
                .values
                .get(variant)
                .ok_or_else(|| format!("MIR variant predicate source '{}' is absent", variant))?
                .ty
                .clone();
            let receipt = contract
                .as_ref()
                .ok_or_else(|| "MIR variant predicate has no canonical receipt".to_string())?;
            catalog.validate_variant_predicate_receipt(
                &result_ty,
                &variant_ty,
                *predicate,
                receipt,
            )?;
            let value = state.values.get(variant).cloned().ok_or_else(|| {
                format!("MIR variant predicate source '{}' is not defined", variant)
            })?;
            let SymbolicValue::Variant { nominal, tag, .. } = value else {
                return Err("MIR variant predicate source is not symbolic Option/Result".into());
            };
            if nominal != receipt.nominal {
                return Err("MIR variant predicate nominal disagrees with TypeDesc".into());
            }
            let output = SymbolicValue::Bool(tag.eq(Int::from_i64(receipt.discriminant as i64)));
            ensure_result_shape(function, catalog, result, &output)?;
            state.values.insert(result.clone(), output);
        }
        MirInstructionKind::FlowTransition {
            result,
            transition,
            arguments,
        } => eval_flow_transition(
            function, program, catalog, state, result, transition, arguments,
        )?,
        MirInstructionKind::Borrow {
            result,
            source,
            mutable,
        } => {
            let result_ty = function
                .values
                .get(result)
                .ok_or_else(|| format!("MIR borrow result '{}' is absent", result))?
                .ty
                .clone();
            let source_ty = function
                .values
                .get(source)
                .ok_or_else(|| format!("MIR borrow source '{}' is absent", source))?
                .ty
                .clone();
            catalog.validate_borrow(&source_ty, &result_ty, *mutable)?;
            let value = state
                .values
                .get(source)
                .cloned()
                .ok_or_else(|| format!("MIR borrow source '{}' is not defined", source))?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::EndBorrow { borrow } => {
            let borrow_ty = function
                .values
                .get(borrow)
                .ok_or_else(|| format!("MIR end-borrow value '{}' is absent", borrow))?
                .ty
                .clone();
            catalog.validate_reference_type(&borrow_ty)?;
            if state.values.remove(borrow).is_none() {
                return Err(format!("MIR end-borrow value '{}' is not defined", borrow));
            }
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

fn instruction_value_type(
    function: &MirFunction,
    value: &MirValueId,
    role: &str,
) -> Result<crate::core::ResolvedTypeId, String> {
    function
        .values
        .get(value)
        .map(|info| info.ty.clone())
        .ok_or_else(|| format!("MIR verifier {role} '{}' is absent", value))
}

fn ensure_same_instruction_types(
    function: &MirFunction,
    result: &MirValueId,
    source_ty: &crate::core::ResolvedTypeId,
    operation: &str,
) -> Result<(), String> {
    let result_ty = instruction_value_type(function, result, &format!("{operation} result"))?;
    if result_ty != *source_ty {
        return Err(format!(
            "MIR verifier {operation} result TypeDesc '{}' disagrees with source TypeDesc '{}'",
            result_ty.as_str(),
            source_ty.as_str()
        ));
    }
    Ok(())
}

fn eval_flow_transition(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &MirValueId,
    transition: &crate::core::NodeId,
    arguments: &[MirValueId],
) -> Result<(), String> {
    let contract = program.transitions().get(transition).ok_or_else(|| {
        format!(
            "MIR verifier transition '{}' has no canonical contract",
            transition.0
        )
    })?;
    if contract.effect != crate::core::mir::MirTransitionEffect::SilentLocal
        || contract.targets.len() != 1
        || contract.failure.is_some()
        || contract.is_fallback
        || contract.is_ffi_pinned
        || contract.targets.first() != Some(&contract.result)
    {
        return Err(format!(
            "MIR verifier transition '{}' is outside the silent-local contract",
            transition.0
        ));
    }
    let target = program.functions().get(&contract.owner).ok_or_else(|| {
        format!(
            "MIR verifier transition '{}' executable body is absent",
            transition.0
        )
    })?;
    if arguments.len() != target.parameters.len() {
        return Err(format!(
            "MIR verifier transition '{}' argument arity disagrees with its body",
            transition.0
        ));
    }
    if function
        .values
        .get(result)
        .is_none_or(|value| value.ty != target.result)
    {
        return Err(format!(
            "MIR verifier transition '{}' result TypeDesc disagrees with its body",
            transition.0
        ));
    }

    let mut target_state = SymbolicState {
        values: BTreeMap::new(),
        constraints: state.constraints.clone(),
        traps: Vec::new(),
    };
    for (argument, parameter) in arguments.iter().zip(&target.parameters) {
        let argument_info = function.values.get(argument).ok_or_else(|| {
            format!(
                "MIR verifier transition '{}' argument '{}' is absent",
                transition.0, argument
            )
        })?;
        let parameter_info = target.values.get(parameter).ok_or_else(|| {
            format!(
                "MIR verifier transition '{}' parameter '{}' is absent",
                transition.0, parameter
            )
        })?;
        if argument_info.ty != parameter_info.ty {
            return Err(format!(
                "MIR verifier transition '{}' argument TypeDesc disagrees with its body",
                transition.0
            ));
        }
        let value = state.values.get(argument).cloned().ok_or_else(|| {
            format!(
                "MIR verifier transition '{}' argument '{}' is not defined",
                transition.0, argument
            )
        })?;
        target_state.values.insert(parameter.clone(), value);
    }
    for (argument, parameter) in arguments.iter().zip(&target.parameters) {
        let ty = target
            .values
            .get(parameter)
            .map(|value| value.ty.clone())
            .ok_or_else(|| "MIR verifier transition parameter TypeDesc is absent".to_string())?;
        if catalog
            .get(&ty)
            .is_some_and(|descriptor| descriptor.ownership != MirOwnership::Copy)
        {
            catalog.validate_glue(&ty, MirGlueOperation::MoveOut)?;
            state.values.remove(argument).ok_or_else(|| {
                format!(
                    "MIR verifier transition '{}' argument '{}' is not available for move",
                    transition.0, argument
                )
            })?;
        }
    }

    let mut returns = Vec::new();
    let mut traps = Vec::new();
    explore_block(
        target,
        program,
        catalog,
        &mut target_state,
        &target.entry,
        &mut BTreeSet::new(),
        &mut returns,
        &mut traps,
    )?;
    if !traps.is_empty() {
        return Err(format!(
            "MIR verifier transition '{}' has a trapping execution path",
            transition.0
        ));
    }
    let [returned] = returns.as_slice() else {
        return Err(format!(
            "MIR verifier transition '{}' must have exactly one non-trapping return path",
            transition.0
        ));
    };
    state.constraints = returned.constraints.clone();
    ensure_result_shape(function, catalog, result, &returned.value)?;
    state.values.insert(result.clone(), returned.value.clone());
    Ok(())
}

fn eval_materialized_call(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &Option<MirValueId>,
    callee: &crate::core::ir::ResolvedCallee,
    type_arguments: &[crate::core::ir::ResolvedTypeId],
    arguments: &[MirValueId],
    variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
) -> Result<(), String> {
    let crate::core::ir::ResolvedCallee::Function(target_owner) = callee else {
        return Err("MIR verifier call callee is not a canonical function instance".into());
    };
    let Some(instance) = program
        .instances()
        .values()
        .find(|instance| instance.function == *target_owner)
    else {
        let target = program.functions().get(target_owner).ok_or_else(|| {
            format!(
                "MIR verifier direct call target '{}' is absent from canonical MIR",
                target_owner.0
            )
        })?;
        if catalog.validate_owned_string(&target.result).is_ok() {
            return eval_direct_owned_string_call(
                function,
                program,
                catalog,
                state,
                result,
                target_owner,
                type_arguments,
                arguments,
            );
        }
        return eval_direct_variant_call(
            function,
            program,
            catalog,
            state,
            result,
            target_owner,
            type_arguments,
            arguments,
            variant_call_contract,
        );
    };
    if instance.arguments != type_arguments {
        return Err(format!(
            "MIR verifier call target '{}' disagrees with its instance arguments",
            target_owner.0
        ));
    }
    match &instance.contract {
        crate::core::mir::MirGenericInstanceContract::ScalarIdentity
        | crate::core::mir::MirGenericInstanceContract::OwnedStringIdentity => {
            eval_materialized_identity_call(
                function,
                program,
                catalog,
                state,
                result,
                callee,
                type_arguments,
                arguments,
                variant_call_contract,
            )
        }
        crate::core::mir::MirGenericInstanceContract::ScalarSetFacade { operation } => {
            eval_materialized_set_facade_call(
                function,
                program,
                catalog,
                state,
                result,
                target_owner,
                type_arguments,
                arguments,
                *operation,
            )
        }
        crate::core::mir::MirGenericInstanceContract::ScalarListFacade { operation } => {
            eval_materialized_list_facade_call(
                function,
                program,
                catalog,
                state,
                result,
                target_owner,
                type_arguments,
                arguments,
                *operation,
            )
        }
        crate::core::mir::MirGenericInstanceContract::ScalarListConstruct { contract } => {
            eval_materialized_list_construct_call(
                function,
                program,
                catalog,
                state,
                result,
                target_owner,
                type_arguments,
                arguments,
                contract,
            )
        }
        crate::core::mir::MirGenericInstanceContract::ScalarListProjection {
            contract,
            index_value,
        } => eval_materialized_list_projection_call(
            function,
            program,
            catalog,
            state,
            result,
            target_owner,
            type_arguments,
            arguments,
            contract,
            *index_value,
        ),
    }
}

/// Symbolically execute a concrete call whose callee returns the canonical
/// owned String.  The callee body is already in Canonical MIR; this helper
/// validates its one-block String return ledger, transfers non-Copy arguments
/// out of the caller state, and reuses the same MIR explorer as the caller.
/// No source body, LLVM ABI, or legacy verifier path participates here.
fn eval_direct_owned_string_call(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &Option<MirValueId>,
    target_owner: &crate::core::NodeId,
    type_arguments: &[crate::core::ResolvedTypeId],
    arguments: &[MirValueId],
) -> Result<(), String> {
    if !type_arguments.is_empty() {
        return Err(
            "MIR verifier direct owned String call cannot carry generic type arguments".into(),
        );
    }
    let target = program.functions().get(target_owner).ok_or_else(|| {
        format!(
            "MIR verifier direct owned String call target '{}' is absent",
            target_owner.0
        )
    })?;
    catalog.validate_owned_string(&target.result)?;
    crate::core::mir::validate_owned_string_return_shape(target, catalog).map_err(|message| {
        format!(
            "MIR verifier direct owned String call target '{}' rejected: {message}",
            target_owner.0
        )
    })?;
    if arguments.len() != target.parameters.len() {
        return Err("MIR verifier direct owned String call arity disagrees with target".into());
    }
    let result = result
        .as_ref()
        .ok_or_else(|| "MIR verifier direct owned String call must produce a result".to_string())?;
    if function
        .values
        .get(result)
        .is_none_or(|value| value.ty != target.result)
    {
        return Err(
            "MIR verifier direct owned String call result disagrees with target TypeDesc".into(),
        );
    }

    let caller_constraints = state.constraints.clone();
    let mut target_state = SymbolicState {
        values: BTreeMap::new(),
        constraints: caller_constraints.clone(),
        traps: Vec::new(),
    };
    for (argument, parameter) in arguments.iter().zip(&target.parameters) {
        let argument_info = function.values.get(argument).ok_or_else(|| {
            format!(
                "MIR verifier direct owned String call argument '{}' is absent",
                argument
            )
        })?;
        let parameter_info = target.values.get(parameter).ok_or_else(|| {
            format!(
                "MIR verifier direct owned String call parameter '{}' is absent",
                parameter
            )
        })?;
        if argument_info.ty != parameter_info.ty {
            return Err(
                "MIR verifier direct owned String call argument disagrees with target TypeDesc"
                    .into(),
            );
        }
        let is_non_copy = catalog
            .get(&argument_info.ty)
            .is_some_and(|descriptor| descriptor.ownership != MirOwnership::Copy);
        let value = if is_non_copy {
            state.values.remove(argument).ok_or_else(|| {
                format!(
                    "MIR verifier direct owned String call argument '{}' is not defined",
                    argument
                )
            })?
        } else {
            state.values.get(argument).cloned().ok_or_else(|| {
                format!(
                    "MIR verifier direct owned String call argument '{}' is not defined",
                    argument
                )
            })?
        };
        if is_non_copy {
            catalog.validate_glue(&argument_info.ty, MirGlueOperation::MoveOut)?;
        }
        if !symbolic_matches_type(catalog, &parameter_info.ty, &value) {
            return Err(
                "MIR verifier direct owned String call argument has the wrong symbolic shape"
                    .into(),
            );
        }
        target_state.values.insert(parameter.clone(), value);
    }

    let mut returns = Vec::new();
    let mut traps = Vec::new();
    explore_block(
        target,
        program,
        catalog,
        &mut target_state,
        &target.entry,
        &mut BTreeSet::new(),
        &mut returns,
        &mut traps,
    )?;
    if !traps.is_empty() {
        return Err("MIR verifier direct owned String call has a trapping execution path".into());
    }
    let [returned] = returns.as_slice() else {
        return Err(
            "MIR verifier direct owned String call must have exactly one return path".into(),
        );
    };
    state.constraints = returned.constraints.clone();
    ensure_result_shape(function, catalog, result, &returned.value)?;
    state.values.insert(result.clone(), returned.value.clone());
    Ok(())
}

/// Symbolically execute the narrow direct-call ABI island for a flat Copy
/// Option/Result or move-owned `Result<string, i32>` result. The callee is
/// already concrete MIR, so the verifier maps symbolic arguments into its
/// entry block and reuses `explore_block`; it does not inspect a surface body
/// or rediscover a call ABI. Ownership-bearing Result paths use the explicit
/// path-exclusive merge contract carried by the canonical receipt.
fn eval_direct_variant_call(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &Option<MirValueId>,
    target_owner: &crate::core::NodeId,
    type_arguments: &[crate::core::ResolvedTypeId],
    arguments: &[MirValueId],
    receipt: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
) -> Result<(), String> {
    let target = program.functions().get(target_owner).ok_or_else(|| {
        format!(
            "MIR verifier direct call target '{}' is absent from canonical MIR",
            target_owner.0
        )
    })?;
    if !type_arguments.is_empty() {
        return Err("MIR verifier direct variant call cannot carry generic type arguments".into());
    }
    let result = result
        .as_ref()
        .ok_or_else(|| "MIR verifier direct variant call must produce a result".to_string())?;
    let result_ty = function
        .values
        .get(result)
        .ok_or_else(|| format!("MIR verifier direct call result '{}' is absent", result))?;
    if result_ty.ty != target.result {
        return Err(
            "MIR verifier direct variant call result disagrees with target TypeDesc".into(),
        );
    }
    let parameter_types = target
        .parameters
        .iter()
        .map(|parameter| {
            target
                .values
                .get(parameter)
                .map(|value| value.ty.clone())
                .ok_or_else(|| "MIR verifier direct call parameter TypeDesc is absent".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if arguments.len() != parameter_types.len() {
        return Err("MIR verifier direct variant call arity disagrees with target".into());
    }
    let flat_variant_result = catalog.validate_flat_copy_variant(&target.result).is_ok();
    let move_owned_result = catalog
        .validate_result_string_i32_variant(&target.result)
        .is_ok();
    if !flat_variant_result && !move_owned_result {
        return Err(
            "MIR verifier direct variant call result is outside the canonical call ABI contract"
                .into(),
        );
    }
    let receipt = receipt.ok_or_else(|| {
        if flat_variant_result {
            "MIR verifier flat Copy variant call has no canonical ABI receipt".to_string()
        } else {
            "MIR verifier move-owned Result<string, i32> call has no canonical ABI receipt"
                .to_string()
        }
    })?;
    catalog.validate_variant_call_abi_receipt(
        target_owner,
        type_arguments,
        &parameter_types,
        &target.result,
        receipt,
    )?;
    if move_owned_result {
        crate::core::mir::validate_move_owned_result_return_merge(target, catalog)?;
    } else {
        crate::core::mir::validate_variant_call_return_coverage(target)?;
    }

    let caller_constraints = state.constraints.clone();
    let mut target_state = SymbolicState {
        values: BTreeMap::new(),
        constraints: caller_constraints.clone(),
        traps: Vec::new(),
    };
    for ((argument, parameter), parameter_ty) in arguments
        .iter()
        .zip(&target.parameters)
        .zip(&parameter_types)
    {
        let argument_value = function
            .values
            .get(argument)
            .ok_or_else(|| format!("MIR verifier direct call argument '{}' is absent", argument))?;
        if argument_value.ty != *parameter_ty {
            return Err(
                "MIR verifier direct variant call argument TypeDesc disagrees with target".into(),
            );
        }
        let symbolic = if !move_owned_result {
            ensure_copy_value(function, catalog, argument)?;
            state.values.get(argument).cloned().ok_or_else(|| {
                format!(
                    "MIR verifier direct call argument '{}' is not defined",
                    argument
                )
            })?
        } else {
            let argument_is_copy = catalog
                .get(&argument_value.ty)
                .is_some_and(|descriptor| descriptor.ownership == MirOwnership::Copy);
            if argument_is_copy {
                state.values.get(argument).cloned().ok_or_else(|| {
                    format!(
                        "MIR verifier direct call argument '{}' is not defined",
                        argument
                    )
                })?
            } else {
                catalog.validate_glue(&argument_value.ty, MirGlueOperation::MoveOut)?;
                state.values.remove(argument).ok_or_else(|| {
                    format!(
                        "MIR verifier direct call argument '{}' is not available for move",
                        argument
                    )
                })?
            }
        };
        if !symbolic_matches_type(catalog, parameter_ty, &symbolic) {
            return Err("MIR verifier direct call argument has the wrong symbolic shape".into());
        }
        target_state.values.insert(parameter.clone(), symbolic);
    }

    let mut returns = Vec::new();
    let mut traps = Vec::new();
    explore_block(
        target,
        program,
        catalog,
        &mut target_state,
        &target.entry,
        &mut BTreeSet::new(),
        &mut returns,
        &mut traps,
    )?;
    if !traps.is_empty() {
        return Err("MIR verifier direct variant call has a trapping execution path".into());
    }
    let returned = if move_owned_result {
        merge_move_owned_result_return_paths(catalog, &target.result, &returns)?
    } else {
        merge_direct_variant_return_paths(catalog, &target.result, &returns)?
    };
    // Callee branch conditions are embedded in the merged symbolic variant;
    // only the caller's constraints remain at this program point.  Copying a
    // single callee path here would unsoundly constrain the caller to one
    // branch and make the proof depend on return-path enumeration order.
    state.constraints = caller_constraints;
    ensure_result_shape(function, catalog, result, &returned)?;
    state.values.insert(result.clone(), returned);
    Ok(())
}

/// Merge the complete return set of a direct flat Copy variant call into one
/// symbolic value.  Every path is structurally covered before this function
/// runs; the path condition selects the canonical tag and each scalar payload
/// slot.  Missing payload fields on a zero-payload variant receive a typed
/// dummy value because they are semantically inactive under that tag.
fn merge_direct_variant_return_paths(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    result_ty: &crate::core::ResolvedTypeId,
    returns: &[ReturnPath],
) -> Result<SymbolicValue, String> {
    if returns.is_empty() {
        return Err("MIR verifier direct variant call has no return paths to merge".into());
    }
    catalog.validate_flat_copy_variant(result_ty)?;
    let Some((expected_nominal, variants)) = catalog.variant_layout(result_ty) else {
        return Err("MIR verifier direct variant call has no canonical variant layout".into());
    };
    let nominal =
        crate::core::ir::NominalTypeId::new(expected_nominal).map_err(|error| error.to_string())?;
    let all_fields = variants
        .iter()
        .flat_map(|variant| variant.fields.iter())
        .map(|field| (field.id.clone(), field.ty.clone()))
        .collect::<Vec<_>>();
    let mut normalized = Vec::with_capacity(returns.len());
    for path in returns {
        let value = normalize_direct_variant_return(
            catalog,
            result_ty,
            &nominal,
            &all_fields,
            &path.value,
        )?;
        normalized.push((conjunction(&path.constraints), value));
    }
    let (_, last) = normalized
        .pop()
        .expect("validated non-empty direct variant return paths");
    let mut merged = last;
    for (condition, value) in normalized.into_iter().rev() {
        merged = merge_symbolic_variants(&condition, value, merged)?;
    }
    Ok(merged)
}

/// Merge ownership-bearing `Result<string, i32>` returns after the canonical
/// MIR path validator has proved that every reachable path is total and
/// non-trapping.  The String payload is intentionally opaque to Z3, so the
/// merge preserves its TypeDesc identity while the Copy `i32` payload remains
/// symbolically selectable by the path condition.
fn merge_move_owned_result_return_paths(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    result_ty: &crate::core::ResolvedTypeId,
    returns: &[ReturnPath],
) -> Result<SymbolicValue, String> {
    if returns.is_empty() {
        return Err("MIR verifier direct variant call has no return paths to merge".into());
    }
    catalog.validate_result_string_i32_variant(result_ty)?;
    let Some((expected_nominal, variants)) = catalog.variant_layout(result_ty) else {
        return Err("MIR verifier direct variant call has no canonical Result layout".into());
    };
    let nominal =
        crate::core::ir::NominalTypeId::new(expected_nominal).map_err(|error| error.to_string())?;
    let all_fields = variants
        .iter()
        .flat_map(|variant| variant.fields.iter())
        .map(|field| (field.id.clone(), field.ty.clone()))
        .collect::<Vec<_>>();
    let mut normalized = Vec::with_capacity(returns.len());
    for path in returns {
        let value = normalize_direct_variant_return(
            catalog,
            result_ty,
            &nominal,
            &all_fields,
            &path.value,
        )?;
        normalized.push((conjunction(&path.constraints), value));
    }
    let (_, last) = normalized
        .pop()
        .expect("validated non-empty move-owned Result return paths");
    let mut merged = last;
    for (condition, value) in normalized.into_iter().rev() {
        merged = merge_symbolic_variants(&condition, value, merged)?;
    }
    Ok(merged)
}

fn normalize_direct_variant_return(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    result_ty: &crate::core::ResolvedTypeId,
    expected_nominal: &crate::core::ir::NominalTypeId,
    all_fields: &[(crate::core::NodeId, crate::core::ResolvedTypeId)],
    value: &SymbolicValue,
) -> Result<SymbolicValue, String> {
    let SymbolicValue::Variant {
        nominal,
        tag,
        payload,
        ..
    } = value
    else {
        return Err(
            "MIR verifier direct variant call returned a non-variant symbolic value".into(),
        );
    };
    if nominal != expected_nominal || !symbolic_matches_type(catalog, result_ty, value) {
        return Err(
            "MIR verifier direct variant call return disagrees with canonical TypeDesc".into(),
        );
    }
    let mut normalized_payload = BTreeMap::new();
    for (field, field_ty) in all_fields {
        let field_value = payload
            .get(field)
            .cloned()
            .unwrap_or(symbolic_zero_for_type(catalog, field_ty)?);
        if !symbolic_matches_type(catalog, field_ty, &field_value) {
            return Err(format!(
                "MIR verifier direct variant call payload field '{}' disagrees with TypeDesc",
                field.0
            ));
        }
        normalized_payload.insert(field.clone(), field_value);
    }
    Ok(SymbolicValue::Variant {
        nominal: expected_nominal.clone(),
        tag: tag.clone(),
        payload: normalized_payload,
        active_variant: None,
    })
}

fn symbolic_zero_for_type(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<SymbolicValue, String> {
    let descriptor = catalog
        .get(ty)
        .ok_or_else(|| format!("MIR verifier payload TypeDesc '{}' is absent", ty.as_str()))?;
    if descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String) {
        catalog.validate_owned_string(ty)?;
        return Ok(SymbolicValue::Opaque { ty: ty.clone() });
    }
    match descriptor.abi {
        MirAbiClass::Integer {
            bits: 32 | 64,
            signed: true,
        } if descriptor.layout == MirLayout::Scalar => Ok(SymbolicValue::Int(Int::from_i64(0))),
        MirAbiClass::Bool if descriptor.layout == MirLayout::Scalar => {
            Ok(SymbolicValue::Bool(Bool::from_bool(false)))
        }
        _ => Err(format!(
            "MIR verifier direct variant call payload TypeDesc '{}' has no scalar zero value",
            ty.as_str()
        )),
    }
}

fn merge_symbolic_variants(
    condition: &Bool,
    when_true: SymbolicValue,
    when_false: SymbolicValue,
) -> Result<SymbolicValue, String> {
    let (
        SymbolicValue::Variant {
            nominal: true_nominal,
            tag: true_tag,
            payload: true_payload,
            ..
        },
        SymbolicValue::Variant {
            nominal: false_nominal,
            tag: false_tag,
            payload: false_payload,
            ..
        },
    ) = (when_true, when_false)
    else {
        return Err("MIR verifier direct variant call merge requires variant return paths".into());
    };
    if true_nominal != false_nominal || true_payload.len() != false_payload.len() {
        return Err(
            "MIR verifier direct variant call merge has incompatible nominal/payload".into(),
        );
    }
    let mut payload = BTreeMap::new();
    for (field, true_value) in true_payload {
        let false_value = false_payload.get(&field).cloned().ok_or_else(|| {
            format!(
                "MIR verifier direct variant call merge is missing payload field '{}'",
                field.0
            )
        })?;
        payload.insert(
            field,
            merge_symbolic_scalars(condition, true_value, false_value)?,
        );
    }
    Ok(SymbolicValue::Variant {
        nominal: true_nominal,
        tag: condition.ite(&true_tag, &false_tag),
        payload,
        active_variant: None,
    })
}

fn merge_symbolic_scalars(
    condition: &Bool,
    when_true: SymbolicValue,
    when_false: SymbolicValue,
) -> Result<SymbolicValue, String> {
    match (when_true, when_false) {
        (SymbolicValue::Int(when_true), SymbolicValue::Int(when_false)) => {
            Ok(SymbolicValue::Int(condition.ite(&when_true, &when_false)))
        }
        (SymbolicValue::Bool(when_true), SymbolicValue::Bool(when_false)) => {
            Ok(SymbolicValue::Bool(condition.ite(&when_true, &when_false)))
        }
        (SymbolicValue::Opaque { ty: when_true }, SymbolicValue::Opaque { ty: when_false })
            if when_true == when_false =>
        {
            Ok(SymbolicValue::Opaque { ty: when_true })
        }
        _ => Err("MIR verifier direct variant call merge requires scalar payloads".into()),
    }
}

/// Symbolically consume a materialized scalar Set facade call. The target
/// body is already structurally proven as `Clone*; SetOp; Return` by the MIR
/// instance validator. This helper applies that exact SetOp contract without
/// rediscovering a method name or a backend-specific handle operation.
fn eval_materialized_set_facade_call(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &Option<MirValueId>,
    target_owner: &crate::core::NodeId,
    type_arguments: &[crate::core::ResolvedTypeId],
    arguments: &[MirValueId],
    operation: crate::core::mir::MirSetOperation,
) -> Result<(), String> {
    let target = program.functions().get(target_owner).ok_or_else(|| {
        format!(
            "MIR verifier Set facade target '{}' is absent",
            target_owner.0
        )
    })?;
    crate::core::mir::lower::validate_scalar_set_facade_mir(target, catalog, operation)?;
    catalog.validate_scalar_generic_arguments(type_arguments)?;
    if arguments.len() != target.parameters.len() {
        return Err("MIR verifier Set facade call arity disagrees with target".into());
    }
    let result = result
        .as_ref()
        .ok_or_else(|| "MIR verifier Set facade call must produce a result".to_string())?;
    if function
        .values
        .get(result)
        .is_none_or(|value| value.ty != target.result)
    {
        return Err("MIR verifier Set facade call result disagrees with target TypeDesc".into());
    }
    let mut symbolic_arguments = Vec::with_capacity(arguments.len());
    for (argument, parameter) in arguments.iter().zip(&target.parameters) {
        let argument_ty = function
            .values
            .get(argument)
            .ok_or_else(|| format!("MIR verifier Set facade argument '{}' is absent", argument))?;
        let parameter_ty = target.values.get(parameter).ok_or_else(|| {
            format!(
                "MIR verifier Set facade parameter '{}' is absent",
                parameter
            )
        })?;
        if argument_ty.ty != parameter_ty.ty {
            return Err("MIR verifier Set facade argument disagrees with target TypeDesc".into());
        }
        symbolic_arguments.push(state.values.get(argument).cloned().ok_or_else(|| {
            format!(
                "MIR verifier Set facade argument '{}' is not defined",
                argument
            )
        })?);
    }
    let set_ty = target
        .parameters
        .first()
        .and_then(|parameter| target.values.get(parameter))
        .map(|value| value.ty.clone())
        .ok_or_else(|| "MIR verifier Set facade receiver parameter is absent".to_string())?;
    let receiver = symbolic_arguments
        .first()
        .cloned()
        .ok_or_else(|| "MIR verifier Set facade receiver argument is absent".to_string())?;
    let argument = symbolic_arguments.get(1).cloned();
    let output = symbolic_set_operation(
        catalog,
        state,
        &target.result,
        &set_ty,
        operation,
        receiver,
        argument,
    )?;
    ensure_result_shape(function, catalog, result, &output)?;
    state.values.insert(result.clone(), output);
    Ok(())
}

/// Symbolically consume a materialized scalar List facade call. The target
/// body is already proven as `Clone; ListOp; Return` (or the two-input
/// `Move; Move; ListOp; Return` concat shape); this helper applies
/// that receipt to the caller's symbolic List without reconstructing the
/// generic body from a template or backend ABI.
fn eval_materialized_list_facade_call(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &Option<MirValueId>,
    target_owner: &crate::core::NodeId,
    type_arguments: &[crate::core::ResolvedTypeId],
    arguments: &[MirValueId],
    operation: crate::core::mir::MirListOperation,
) -> Result<(), String> {
    let target = program.functions().get(target_owner).ok_or_else(|| {
        format!(
            "MIR verifier List facade target '{}' is absent",
            target_owner.0
        )
    })?;
    crate::core::mir::lower::validate_scalar_list_facade_mir(target, catalog, operation)?;
    catalog.validate_scalar_generic_arguments(type_arguments)?;
    if arguments.len() != target.parameters.len() {
        return Err("MIR verifier List facade call arity disagrees with target".into());
    }
    let result = result
        .as_ref()
        .ok_or_else(|| "MIR verifier List facade call must produce a result".to_string())?;
    if function
        .values
        .get(result)
        .is_none_or(|value| value.ty != target.result)
    {
        return Err("MIR verifier List facade call result disagrees with target TypeDesc".into());
    }
    let argument = arguments
        .first()
        .ok_or_else(|| "MIR verifier List facade receiver argument is absent".to_string())?;
    let argument_info = function
        .values
        .get(argument)
        .ok_or_else(|| format!("MIR verifier List facade argument '{}' is absent", argument))?;
    let parameter = target
        .parameters
        .first()
        .ok_or_else(|| "MIR verifier List facade target parameter is absent".to_string())?;
    let parameter_info = target
        .values
        .get(parameter)
        .ok_or_else(|| "MIR verifier List facade parameter TypeDesc is absent".to_string())?;
    if argument_info.ty != parameter_info.ty {
        return Err("MIR verifier List facade argument disagrees with target TypeDesc".into());
    }
    let SymbolicValue::List { length } = state.values.get(argument).cloned().ok_or_else(|| {
        format!(
            "MIR verifier List facade argument '{}' is not defined",
            argument
        )
    })?
    else {
        return Err("MIR verifier List facade receiver is not a symbolic List".into());
    };
    let output = match operation {
        crate::core::mir::MirListOperation::Len => {
            add_definedness(state, length.le(Int::from_i64(i32::MAX as i64)), "E0802")?;
            SymbolicValue::Int(length)
        }
        crate::core::mir::MirListOperation::Reverse => SymbolicValue::List { length },
        crate::core::mir::MirListOperation::Concat => {
            let second_argument = arguments
                .get(1)
                .ok_or_else(|| "MIR verifier List.concat facade argument is absent".to_string())?;
            let second_info = function.values.get(second_argument).ok_or_else(|| {
                format!(
                    "MIR verifier List.concat facade argument '{}' is absent",
                    second_argument
                )
            })?;
            let second_parameter = target.parameters.get(1).ok_or_else(|| {
                "MIR verifier List.concat facade target second parameter is absent".to_string()
            })?;
            let second_parameter_info = target.values.get(second_parameter).ok_or_else(|| {
                "MIR verifier List.concat facade second parameter TypeDesc is absent".to_string()
            })?;
            if second_info.ty != second_parameter_info.ty {
                return Err(
                    "MIR verifier List.concat facade argument disagrees with target TypeDesc"
                        .into(),
                );
            }
            let SymbolicValue::List { length: right } =
                state.values.get(second_argument).cloned().ok_or_else(|| {
                    format!(
                        "MIR verifier List.concat facade argument '{}' is not defined",
                        second_argument
                    )
                })?
            else {
                return Err(
                    "MIR verifier List.concat facade argument is not a symbolic List".into(),
                );
            };
            let length = Int::add(&[&length, &right]);
            add_definedness(state, length.le(Int::from_i64(i64::MAX)), "E0800")?;
            // The specialized concat contract moves both call arguments out
            // of the caller.  Keep the caller's source values (which were
            // cloned before the call) untouched, but consume these argument
            // value ids before publishing the fresh result.
            state.values.remove(argument);
            state.values.remove(second_argument);
            SymbolicValue::List { length }
        }
    };
    ensure_result_shape(function, catalog, result, &output)?;
    state.values.insert(result.clone(), output);
    Ok(())
}

/// Symbolically consume a materialized one-element generic List construction.
/// The callee body and its TypeDesc receipt are already validated by the MIR
/// instance gate. The Copy scalar argument remains available to the caller;
/// the fresh List result carries exactly the checker-proven element count.
fn eval_materialized_list_construct_call(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &Option<MirValueId>,
    target_owner: &crate::core::NodeId,
    type_arguments: &[crate::core::ResolvedTypeId],
    arguments: &[MirValueId],
    contract: &crate::core::mir::types::MirListConstructContract,
) -> Result<(), String> {
    let target = program.functions().get(target_owner).ok_or_else(|| {
        format!(
            "MIR verifier List construction target '{}' is absent",
            target_owner.0
        )
    })?;
    crate::core::mir::lower::validate_scalar_list_construct_mir(target, catalog, contract)?;
    catalog.validate_scalar_generic_arguments(type_arguments)?;
    if arguments.len() != 1 || target.parameters.len() != 1 {
        return Err("MIR verifier List construction call requires one argument".into());
    }
    let result = result
        .as_ref()
        .ok_or_else(|| "MIR verifier List construction call must produce a result".to_string())?;
    if function
        .values
        .get(result)
        .is_none_or(|value| value.ty != target.result)
    {
        return Err(
            "MIR verifier List construction call result disagrees with target TypeDesc".into(),
        );
    }
    let argument = &arguments[0];
    let argument_info = function.values.get(argument).ok_or_else(|| {
        format!(
            "MIR verifier List construction argument '{}' is absent",
            argument
        )
    })?;
    let parameter = &target.parameters[0];
    let parameter_info = target
        .values
        .get(parameter)
        .ok_or_else(|| "MIR verifier List construction parameter TypeDesc is absent".to_string())?;
    if argument_info.ty != parameter_info.ty || argument_info.ty != contract.element_ty {
        return Err("MIR verifier List construction argument disagrees with TypeDesc".into());
    }
    catalog.validate_glue(&argument_info.ty, MirGlueOperation::Clone)?;
    let value = state.values.get(argument).cloned().ok_or_else(|| {
        format!(
            "MIR verifier List construction argument '{}' is not defined",
            argument
        )
    })?;
    if !symbolic_matches_type(catalog, &argument_info.ty, &value) {
        return Err("MIR verifier List construction argument has the wrong symbolic shape".into());
    }
    let output = SymbolicValue::List {
        length: Int::from_i64(contract.element_count as i64),
    };
    ensure_result_shape(function, catalog, result, &output)?;
    state.values.insert(result.clone(), output);
    Ok(())
}

/// Symbolically consume a materialized generic List constant-index projection.
/// The List argument is borrowed (the caller remains responsible for its Drop)
/// and the result is a fresh symbolic Copy element. The target body and
/// receipt are revalidated so this helper cannot widen into arbitrary dynamic
/// or managed-element projection semantics.
fn eval_materialized_list_projection_call(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &Option<MirValueId>,
    target_owner: &crate::core::NodeId,
    type_arguments: &[crate::core::ResolvedTypeId],
    arguments: &[MirValueId],
    contract: &crate::core::mir::types::MirListIndexProjectionContract,
    index_value: i64,
) -> Result<(), String> {
    let target = program.functions().get(target_owner).ok_or_else(|| {
        format!(
            "MIR verifier List projection target '{}' is absent",
            target_owner.0
        )
    })?;
    crate::core::mir::lower::validate_scalar_list_projection_mir(
        target,
        catalog,
        contract,
        index_value,
    )?;
    catalog.validate_scalar_generic_arguments(type_arguments)?;
    if arguments.len() != 1 || target.parameters.len() != 1 {
        return Err("MIR verifier List projection call requires one argument".into());
    }
    let result = result
        .as_ref()
        .ok_or_else(|| "MIR verifier List projection call must produce a result".to_string())?;
    if function
        .values
        .get(result)
        .is_none_or(|value| value.ty != target.result)
    {
        return Err(
            "MIR verifier List projection call result disagrees with target TypeDesc".into(),
        );
    }
    let argument = &arguments[0];
    let argument_info = function.values.get(argument).ok_or_else(|| {
        format!(
            "MIR verifier List projection argument '{}' is absent",
            argument
        )
    })?;
    let parameter = &target.parameters[0];
    let parameter_info = target
        .values
        .get(parameter)
        .ok_or_else(|| "MIR verifier List projection parameter TypeDesc is absent".to_string())?;
    if argument_info.ty != parameter_info.ty || argument_info.ty != contract.list_ty {
        return Err("MIR verifier List projection argument disagrees with TypeDesc".into());
    }
    let SymbolicValue::List { length } = state.values.get(argument).cloned().ok_or_else(|| {
        format!(
            "MIR verifier List projection argument '{}' is not defined",
            argument
        )
    })?
    else {
        return Err("MIR verifier List projection argument is not a symbolic List".into());
    };
    let zero = Int::from_i64(0);
    add_definedness(state, zero.lt(&length), "E0803")?;
    let (output, constraints) = symbolic_value_for_type(
        catalog,
        &contract.result_ty,
        &format!("mir.list_projection.{}", result),
    )?;
    state.constraints.extend(constraints);
    ensure_result_shape(function, catalog, result, &output)?;
    state.values.insert(result.clone(), output);
    Ok(())
}

/// Symbolically consume the concrete generic identity call admitted by this
/// slice: Copy scalars plus flat Copy Option/Result values.
///
/// The verifier does not infer a callee body from a template name.  It first
/// requires the call to name an instance in the canonical instance table,
/// then checks that the executable target still has the exact specialized
/// one-block or total branch identity shape produced by MIR lowering.  This keeps
/// the proof tied to the same TypeDesc, instance identity, and ownership
/// contract consumed by the reference, bytecode, and native backends.
fn eval_materialized_identity_call(
    function: &MirFunction,
    program: &MirProgram,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    state: &mut SymbolicState,
    result: &Option<MirValueId>,
    callee: &crate::core::ir::ResolvedCallee,
    type_arguments: &[crate::core::ir::ResolvedTypeId],
    arguments: &[MirValueId],
    variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
) -> Result<(), String> {
    let crate::core::ir::ResolvedCallee::Function(target_owner) = callee else {
        return Err("MIR verifier generic call callee is not a canonical function instance".into());
    };
    if type_arguments.len() != 1 || arguments.len() != 1 {
        return Err("MIR verifier only admits one-argument concrete generic identity calls".into());
    }
    let instance = program
        .instances()
        .values()
        .find(|instance| instance.function == *target_owner)
        .ok_or_else(|| {
            format!(
                "MIR verifier generic call target '{}' is absent from the instance table",
                target_owner.0
            )
        })?;
    if instance.arguments != type_arguments {
        return Err(format!(
            "MIR verifier generic call target '{}' disagrees with its instance arguments",
            target_owner.0
        ));
    }
    catalog.validate_generic_identity_arguments(type_arguments)?;

    let target = program
        .functions()
        .get(target_owner)
        .ok_or_else(|| format!("MIR verifier generic target '{}' is absent", target_owner.0))?;
    let flat_variant_result = catalog.validate_flat_copy_variant(&target.result).is_ok();
    if flat_variant_result {
        let receipt = variant_call_contract.ok_or_else(|| {
            "MIR verifier generic flat Copy variant call has no canonical ABI receipt".to_string()
        })?;
        let parameter_types = target
            .parameters
            .iter()
            .map(|parameter| {
                target
                    .values
                    .get(parameter)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        "MIR verifier generic target parameter TypeDesc is absent".to_string()
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        catalog.validate_variant_call_abi_receipt(
            target_owner,
            type_arguments,
            &parameter_types,
            &target.result,
            receipt,
        )?;
    } else if variant_call_contract.is_some() {
        return Err(
            "MIR verifier generic variant call receipt is attached to a non-variant result".into(),
        );
    }
    let [target_parameter] = target.parameters.as_slice() else {
        return Err("MIR verifier generic identity target must have one parameter".into());
    };
    let concrete = type_arguments
        .first()
        .expect("validated one generic type argument");
    let is_owned_string_identity = catalog.validate_owned_string(concrete).is_ok();
    if is_owned_string_identity {
        crate::core::mir::validate_owned_string_identity_shape(target, concrete)?;
    } else {
        crate::core::mir::validate_generic_identity_shape(target, concrete)?;
    }
    let target_parameter_ty = target
        .values
        .get(target_parameter)
        .ok_or_else(|| {
            format!(
                "MIR verifier generic target parameter '{}' is absent",
                target_parameter
            )
        })?
        .ty
        .clone();
    if target_parameter_ty != *concrete || target.result != *concrete {
        return Err(
            "MIR verifier generic identity target is not specialized to its instance argument"
                .into(),
        );
    }
    let argument = arguments
        .first()
        .expect("validated one generic call argument");
    let argument_info = function
        .values
        .get(argument)
        .ok_or_else(|| format!("MIR generic call argument '{}' is absent", argument))?;
    if argument_info.ty != target_parameter_ty {
        return Err("MIR verifier generic call argument disagrees with target TypeDesc".into());
    }
    let symbolic = state
        .values
        .get(argument)
        .cloned()
        .ok_or_else(|| format!("MIR generic call argument '{}' is not defined", argument))?;
    if catalog.validate_owned_string(concrete).is_err() {
        ensure_copy_value(function, catalog, argument)?;
    } else {
        catalog.validate_glue(concrete, MirGlueOperation::Clone)?;
    }
    if !symbolic_matches_type(catalog, concrete, &symbolic) {
        return Err("MIR verifier generic call argument has the wrong concrete Copy shape".into());
    }
    let result = result.as_ref().ok_or_else(|| {
        "MIR verifier generic identity call must produce its canonical result".to_string()
    })?;
    if function
        .values
        .get(result)
        .is_none_or(|value| value.ty != target.result)
    {
        return Err("MIR verifier generic call result disagrees with target TypeDesc".into());
    }
    let caller_constraints = state.constraints.clone();
    if target.blocks.len() == 1 {
        ensure_result_shape(function, catalog, result, &symbolic)?;
        state.values.insert(result.clone(), symbolic);
        return Ok(());
    }
    if !flat_variant_result {
        return Err(
            "MIR verifier generic identity multi-path merge only admits flat Copy Option/Result"
                .into(),
        );
    }
    crate::core::mir::validate_variant_call_return_coverage(target)?;
    let mut target_state = SymbolicState {
        values: BTreeMap::from([(target_parameter.clone(), symbolic)]),
        constraints: caller_constraints.clone(),
        traps: Vec::new(),
    };
    let mut returns = Vec::new();
    let mut traps = Vec::new();
    explore_block(
        target,
        program,
        catalog,
        &mut target_state,
        &target.entry,
        &mut BTreeSet::new(),
        &mut returns,
        &mut traps,
    )?;
    if !traps.is_empty() {
        return Err("MIR verifier generic identity call has a trapping execution path".into());
    }
    let merged = merge_direct_variant_return_paths(catalog, &target.result, &returns)?;
    state.constraints = caller_constraints;
    ensure_result_shape(function, catalog, result, &merged)?;
    state.values.insert(result.clone(), merged);
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
        (MirLayout::Unit, MirAbiClass::Unit, SymbolicValue::Unit) => true,
        (
            MirLayout::Scalar,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            },
            SymbolicValue::Int(_),
        )
        | (MirLayout::Scalar, MirAbiClass::Bool, SymbolicValue::Bool(_)) => true,
        (MirLayout::Handle, MirAbiClass::StringHandle, SymbolicValue::Opaque { ty: actual_ty }) => {
            actual_ty == ty && catalog.validate_owned_string(ty).is_ok()
        }
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
        (MirLayout::Set { .. }, MirAbiClass::SetHandle, SymbolicValue::Set { .. }) => catalog
            .validate_set_glue(ty, MirGlueOperation::MoveOut)
            .is_ok(),
        (MirLayout::List { .. }, MirAbiClass::OpaqueHandle, SymbolicValue::List { .. }) => catalog
            .validate_list_glue(ty, MirGlueOperation::MoveOut)
            .is_ok(),
        (
            MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. },
            MirAbiClass::Aggregate,
            SymbolicValue::Variant {
                nominal: actual_nominal,
                tag: _,
                payload,
                active_variant,
            },
        ) => {
            let expected_nominal = if matches!(&descriptor.layout, MirLayout::Option { .. }) {
                "builtin:type:Option"
            } else {
                "builtin:type:Result"
            };
            let fields_match = if let Some(active_variant) = active_variant {
                variants
                    .iter()
                    .find(|variant| variant.id == *active_variant)
                    .is_some_and(|variant| {
                        payload.len() == variant.fields.len()
                            && variant.fields.iter().all(|field| {
                                payload.get(&field.id).is_some_and(|value| {
                                    symbolic_matches_type(catalog, &field.ty, value)
                                })
                            })
                    })
            } else {
                let expected_fields = variants
                    .iter()
                    .flat_map(|variant| variant.fields.iter())
                    .collect::<Vec<_>>();
                payload.len() == expected_fields.len()
                    && expected_fields.iter().all(|field| {
                        payload
                            .get(&field.id)
                            .is_some_and(|value| symbolic_matches_type(catalog, &field.ty, value))
                    })
            };
            actual_nominal.as_str() == expected_nominal && fields_match
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
        | SymbolicValue::Unit
        | SymbolicValue::Opaque { .. }
        | SymbolicValue::Tuple(_)
        | SymbolicValue::Record { .. }
        | SymbolicValue::Set { .. }
        | SymbolicValue::List { .. }
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
    use crate::core::mir::MirInstructionKind;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use std::collections::BTreeMap;

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
    fn verifier_materializes_direct_variant_projection_as_an_active_tag_trap() {
        let fixture = crate::core::mir::test_support::direct_variant_projection_fixture();
        let results = verify_program(&fixture.program, "direct-variant-project".into())
            .expect("direct variant projection verification");
        let result = results
            .iter()
            .find(|result| result.func_name == fixture.function.0)
            .expect("project verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Disproven);
        assert!(result.message.contains("trap 'E0800'"), "{result:?}");
    }

    #[test]
    fn verifier_consuming_variant_projection_consumes_symbolic_source_and_traps() {
        let fixture = crate::core::mir::test_support::direct_variant_move_projection_fixture();
        let results = verify_program(&fixture.program, "direct-variant-project-move".into())
            .expect("consuming direct variant projection verification");
        let result = results
            .iter()
            .find(|result| result.func_name == fixture.function.0)
            .expect("project verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Disproven);
        assert!(result.message.contains("trap 'E0800'"), "{result:?}");
    }

    #[test]
    fn verifier_proves_record_move_drop_projection_from_canonical_mir() {
        let fixture = crate::core::mir::test_support::direct_record_move_drop_fixture();
        let results = verify_program(&fixture.program, "record-move-drop-project".into())
            .expect("record move/drop projection verification");
        let result = results
            .iter()
            .find(|result| result.func_name == fixture.function.0)
            .expect("project verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::Proven,
            "{result:?}"
        );
        assert!(result
            .message
            .contains("canonical MIR ensures contract proven"));
    }

    #[test]
    fn verifier_and_reference_oracle_preserve_owned_string_glue_for_scalar_contracts() {
        let source = r#"
            func consume(text: string, n: i32) -> i32 {
                requires: n >= 0
                ensures: result == n
                let cloned = text;
                drop(cloned);
                drop(text);
                let literal = "owned";
                drop(literal);
                n
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
            .find(|owner| owner.0.ends_with("consume"))
            .cloned()
            .expect("consume MIR function");

        let reference_value = MirReferenceInterpreter::new(&program)
            .execute(
                &owner,
                &[
                    MirRuntimeValue::String("input".into()),
                    MirRuntimeValue::Int(41),
                ],
            )
            .expect("reference owned String execution");
        assert_eq!(reference_value, MirRuntimeValue::Int(41));

        let results = verify_program(&program, "owned-string-source-hash".into())
            .expect("verify owned String MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("owned String contract verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        assert!(program
            .functions()
            .get(&owner)
            .expect("owned String function")
            .canonical_text()
            .contains("clone"));
        assert!(program
            .type_catalog()
            .iter()
            .any(|(ty, _)| program.type_catalog().validate_owned_string(ty).is_ok()));
    }

    #[test]
    fn verifier_and_reference_oracle_preserve_recursive_owned_tuple_glue() {
        let source = include_str!("../../tests/fixtures/mir_native_recursive_tuple.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        crate::verifier::validate_mir_capabilities(&program)
            .expect("recursive tuple must be in verifier capability");
        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("consume_nested"))
            .cloned()
            .expect("consume_nested MIR function");

        let reference_value = MirReferenceInterpreter::new(&program)
            .execute(
                &owner,
                &[MirRuntimeValue::Tuple(vec![
                    MirRuntimeValue::Tuple(vec![
                        MirRuntimeValue::String("input".into()),
                        MirRuntimeValue::Int(41),
                    ]),
                    MirRuntimeValue::Bool(true),
                ])],
            )
            .expect("reference recursive tuple execution");
        assert_eq!(reference_value, MirRuntimeValue::Int(42));

        let results = verify_program(&program, "recursive-tuple-source-hash".into())
            .expect("verify recursive tuple MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("recursive tuple contract verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        assert!(result
            .artifact
            .as_ref()
            .is_some_and(|artifact| artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR));
    }

    #[test]
    fn verifier_proves_owned_string_result_without_fallback() {
        let source = r#"
            func echo(text: string) -> string {
                ensures: true
                text
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let results = verify_program(&program, "owned-string-result-source-hash".into())
            .expect("verifier should prove canonical owned String result");
        let result = results
            .iter()
            .find(|result| result.func_name.ends_with("echo"))
            .expect("echo verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        assert!(result
            .message
            .contains("canonical MIR ensures contract proven"));
    }

    #[test]
    fn verifier_proves_direct_owned_string_calls_from_canonical_mir() {
        let source =
            include_str!("../../tests/fixtures/mir_verifier_owned_string_call_return.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("direct owned String calls must lower to canonical MIR");
        let results = verify_program(&program, "owned-string-call-source-hash".into())
            .expect("direct owned String calls must use the MIR verifier");
        for owner in [
            "function:echo",
            "function:forward",
            "function:inner",
            "function:relay",
        ] {
            let result = results
                .iter()
                .find(|result| result.func_name == owner)
                .expect("contract verification result");
            assert_eq!(
                result.status,
                crate::verifier::VerifStatus::Proven,
                "{owner}"
            );
            assert!(result
                .message
                .contains("canonical MIR ensures contract proven"));
        }
    }

    #[test]
    fn verifier_preserves_copy_arguments_across_owned_string_call() {
        let source = r#"
            func render(n: i32) -> string { "rendered" }
            func observe(n: i32) -> string {
                ensures: true
                let text = render(n)
                println(n)
                drop(text)
                "done"
            }
            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let results = verify_program(&program, "owned-string-call-copy-source-hash".into())
            .expect("verifier returns a canonical result");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:observe")
            .expect("observe verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        let value = MirReferenceInterpreter::new(&program)
            .execute(
                &crate::core::NodeId("function:observe".into()),
                &[MirRuntimeValue::Int(7)],
            )
            .expect("reference copy argument preservation");
        assert_eq!(value, MirRuntimeValue::String("done".into()));
    }

    #[test]
    fn verifier_proves_non_copy_record_move_projection_from_canonical_mir() {
        let source = include_str!("../../tests/fixtures/mir_verifier_record_move_projection.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("record MoveProject must be canonical MIR");
        let function = program
            .functions()
            .get(&crate::core::NodeId("function:pick".into()))
            .expect("pick MIR function");
        assert!(function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(instruction.kind, MirInstructionKind::MoveProject { .. })));
        let results = verify_program(&program, "record-move-projection-source-hash".into())
            .expect("record MoveProject verifier result");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:pick")
            .expect("pick verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        let value = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:pick".into()), &[])
            .expect("reference record MoveProject");
        assert_eq!(value, MirRuntimeValue::String("owned".into()));
    }

    #[test]
    fn verifier_rejects_non_copy_record_move_projection_with_non_copy_sibling() {
        let source =
            include_str!("../../tests/fixtures/mir_native_record_move_project_rejected.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("non-Copy sibling must fail before verifier exploration");
        let text = format!("{error:?}");
        assert!(text.contains("explicit move projection contract"), "{text}");
    }

    #[test]
    fn verifier_proves_non_copy_result_string_i32_construction_from_canonical_mir() {
        let source = include_str!("../../tests/fixtures/mir_verifier_result_string_i32.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("Result<string, i32> construction must be canonical MIR");
        program
            .type_catalog()
            .validate_result_string_i32_variant(
                &program
                    .functions()
                    .get(&crate::core::NodeId("function:main".into()))
                    .expect("main MIR")
                    .result,
            )
            .expect("shared Result<string, i32> TypeDesc contract");
        let function = program
            .functions()
            .get(&crate::core::NodeId("function:main".into()))
            .expect("main MIR function");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::ConstructVariantMove { .. }
                )
            })
        }));
        let results = verify_program(&program, "result-string-i32-source-hash".into())
            .expect("Result verifier result");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:main")
            .expect("main verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        let value = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference Result execution");
        assert_eq!(
            value,
            crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal: crate::core::ir::NominalTypeId::new("builtin:type:Result")
                    .expect("Result nominal"),
                variant: crate::core::NodeId("builtin:variant:Result::Ok".into()),
                payload: vec![crate::core::mir::reference::MirRuntimeValue::String(
                    "owned".into(),
                )],
            }
        );
    }

    #[test]
    fn verifier_proves_move_owned_result_call_return_from_canonical_mir() {
        let source = include_str!("../../tests/fixtures/mir_result_string_i32_call_return.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("move-owned Result call/return must be canonical MIR");
        let receipts = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 2);
        assert!(receipts.iter().all(|receipt| {
            receipt.mode == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
        }));

        let results = verify_program(&program, "result-string-i32-call-return-source-hash".into())
            .expect("move-owned Result call/return verifier result");
        for owner in ["function:use_ok", "function:use_err"] {
            let result = results
                .iter()
                .find(|result| result.func_name == owner)
                .expect("direct Result call verification result");
            assert_eq!(
                result.status,
                crate::verifier::VerifStatus::Proven,
                "{owner}: {}",
                result.message
            );
            assert!(result
                .message
                .contains("canonical MIR ensures contract proven"));
        }
        let value = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference move-owned Result call/return execution");
        assert_eq!(value, MirRuntimeValue::Int(48));
    }

    #[test]
    fn verifier_proves_move_owned_result_call_with_exclusive_return_paths() {
        let source = r#"
            func choose(flag: bool) -> Result<string, i32> {
                if flag { Ok("owned") } else { Err(1) }
            }

            func checked(flag: bool) -> i32 {
                ensures: result >= 0
                let value = choose(flag)
                match value {
                    Ok(_) => 4,
                    Err(code) => code
                }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("move-owned Result call must lower before verifier");
        let results = verify_program(
            &program,
            "result-string-i32-call-multipath-source-hash".into(),
        )
        .expect("move-owned multi-path call should be classified");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:checked")
            .expect("checked verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::Proven,
            "{}",
            result.message
        );
        assert!(result
            .message
            .contains("canonical MIR ensures contract proven"));
    }

    #[test]
    fn verifier_rejects_non_copy_result_string_string_before_symbolic_execution() {
        let source =
            include_str!("../../tests/fixtures/mir_verifier_result_string_string_rejected.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let errors = crate::verifier::validate_mir_capabilities(&program)
            .expect_err("unsupported Result payload must fail capability gate");
        assert!(errors
            .iter()
            .any(|error| error.contains("non-Copy variant TypeDesc")));
        let results = verify_program(&program, "rejected-result-string-string-source-hash".into())
            .expect("unsupported Result remains a classified verifier result");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:main")
            .expect("main verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(result.message.contains("Result<string, i32>"));
    }

    #[test]
    fn verifier_proves_result_string_i32_consuming_switch_with_shared_receipt() {
        let source =
            include_str!("../../tests/fixtures/mir_verifier_result_string_i32_switch_move.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("Result switch must be canonical MIR");
        let consume_owner = crate::core::NodeId("function:consume".into());
        let consume = program
            .functions()
            .get(&consume_owner)
            .expect("consume MIR function");
        let (scrutinee, arms) = consume
            .blocks
            .values()
            .find_map(|block| match &block.terminator {
                crate::core::mir::MirTerminator::SwitchMove { scrutinee, arms } => {
                    Some((scrutinee, arms))
                }
                _ => None,
            })
            .expect("Result SwitchMove");
        let scrutinee_ty = consume
            .values
            .get(scrutinee)
            .expect("switch scrutinee value")
            .ty
            .clone();
        program
            .type_catalog()
            .validate_variant_switch_move_contract(&scrutinee_ty, arms)
            .expect("shared Result switch-move contract");
        assert!(arms.iter().any(|arm| !arm.bindings.is_empty()));
        assert!(arms.iter().any(|arm| arm.bindings.is_empty()));

        let results = verify_program(&program, "result-string-i32-switch-source-hash".into())
            .expect("Result switch verifier result");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:consume")
            .expect("consume verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        let value = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference Result switch execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn canonical_gate_rejects_result_switch_projection_receipt_drift() {
        let source =
            include_str!("../../tests/fixtures/mir_verifier_result_string_i32_switch_move.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:consume".into());
        let mut function = program
            .functions()
            .get(&owner)
            .cloned()
            .expect("consume MIR");
        let binding = function
            .blocks
            .values_mut()
            .find_map(|block| match &mut block.terminator {
                crate::core::mir::MirTerminator::SwitchMove { arms, .. } => {
                    arms.iter_mut().find_map(|arm| arm.bindings.first_mut())
                }
                _ => None,
            })
            .expect("Result payload binding");
        binding.projection.field_index = 1;
        let errors = MirProgram::with_type_catalog(
            std::collections::BTreeMap::from([(owner, function)]),
            program.type_catalog().clone(),
        )
        .expect_err("stale Result projection receipt must fail before consumers");
        assert!(
            errors.iter().any(|error| {
                error.message.contains("disagrees with TypeDesc")
                    || error.message.contains("outside variant")
                    || error
                        .message
                        .contains("projection index is outside its payload arity")
            }),
            "{errors:?}"
        );
    }

    #[test]
    fn verifier_rejects_nested_owned_string_call_target_without_call_contract() {
        let source = r#"
            func inner() -> string { "inner" }
            func nested() -> string { inner() }
            func outer() -> string {
                ensures: true
                nested()
            }
            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let results = verify_program(&program, "owned-string-call-rejected-source-hash".into())
            .expect("verifier returns a stable trusted-subset result");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:outer")
            .expect("outer verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(result.message.contains(
            "direct owned String call target 'function:nested' rejected: owned String return contract only admits String constants and ownership glue"
        ));
        assert!(!result.message.contains("flow_ast"));
    }

    #[test]
    fn canonical_gate_rejects_owned_string_return_branch_before_verifier() {
        let source = r#"
            func echo(text: string) -> string {
                ensures: true
                if true { text } else { "fallback" }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect_err("branch-shaped owned String return must fail closed");
        match error {
            crate::core::mir::reference::MirProgramBuildError::Validation(errors) => {
                assert!(errors.iter().any(|error| {
                    error
                        .message
                        .contains("owned String return contract requires one canonical MIR block")
                }));
            }
            other => panic!("unsupported owned String return crossed the MIR gate: {other:?}"),
        }
    }

    #[test]
    fn verifier_consumes_materialized_scalar_generic_identity_call() {
        let source = r#"
            func identity<T>(value: T) -> T { value }

            func checked() -> i32 {
                ensures: result == 41
                identity(41)
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert_eq!(program.instances().len(), 1);

        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("checked"))
            .cloned()
            .expect("checked MIR function");
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic call execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let results =
            verify_program(&program, "generic-identity-source-hash".into()).expect("verify MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("generic contract verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
    }

    #[test]
    fn verifier_consumes_materialized_scalar_generic_list_len_call() {
        let source = r#"
            func list_len<T>(values: List<T>) -> i32 { len(values) }

            func checked() -> i32 {
                ensures: result == 3
                let values: List<i32> = [4, 5, 6]
                let count = list_len(values)
                drop(values)
                count
            }

            func main() -> i32 { checked() }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("generic List.len MIR");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List.len instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListFacade {
                operation: crate::core::mir::MirListOperation::Len
            }
        ));
        let results = verify_program(&program, "generic-list-len-source-hash".into())
            .expect("verify generic List.len MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:checked")
            .expect("List.len checked verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::Proven,
            "{}",
            result.message
        );
        assert!(result
            .message
            .contains("canonical MIR ensures contract proven"));
    }

    #[test]
    fn verifier_consumes_materialized_scalar_generic_list_reverse_call() {
        let source = r#"
            func list_reverse<T>(values: List<T>) -> List<T> { values.reverse() }

            func checked() -> i32 {
                ensures: result == 3
                let values: List<i32> = [4, 5, 6]
                let reversed = list_reverse(values)
                let count = len(reversed)
                drop(values)
                drop(reversed)
                count
            }

            func main() -> i32 { checked() }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("generic List.reverse MIR");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List.reverse instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListFacade {
                operation: crate::core::mir::MirListOperation::Reverse
            }
        ));
        let results = verify_program(&program, "generic-list-reverse-source-hash".into())
            .expect("verify generic List.reverse MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:checked")
            .expect("List.reverse checked verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::Proven,
            "{}",
            result.message
        );
    }

    #[test]
    fn verifier_consumes_materialized_scalar_generic_list_concat_call() {
        let source = r#"
            func list_concat<T>(left: List<T>, right: List<T>) -> List<T> {
                left.concat(right)
            }

            func checked() -> i32 {
                ensures: result == 5
                let left: List<i32> = [1, 2]
                let right: List<i32> = [3, 4, 5]
                let joined = list_concat(left, right)
                let count = len(joined)
                drop(left)
                drop(right)
                drop(joined)
                count
            }

            func main() -> i32 { checked() }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("generic List.concat MIR");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List.concat instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListFacade {
                operation: crate::core::mir::MirListOperation::Concat
            }
        ));
        let results = verify_program(&program, "generic-list-concat-source-hash".into())
            .expect("verify generic List.concat MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:checked")
            .expect("List.concat checked verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::Proven,
            "{}",
            result.message
        );
        assert!(result
            .message
            .contains("canonical MIR ensures contract proven"));
    }

    #[test]
    fn verifier_consumes_materialized_scalar_generic_list_construct_call() {
        let source = include_str!("../../tests/fixtures/mir_native_generic_list_construct.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program =
            MirProgram::from_checked_program(&checked).expect("generic List construction MIR");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List construction instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListConstruct { .. }
        ));
        let results = verify_program(&program, "generic-list-construct-source-hash".into())
            .expect("verify generic List construction MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:main")
            .expect("List construction main verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::Proven,
            "{}",
            result.message
        );
        assert!(result
            .message
            .contains("canonical MIR ensures contract proven"));
    }

    #[test]
    fn verifier_consumes_materialized_scalar_generic_list_projection_call() {
        let source = include_str!("../../tests/fixtures/mir_native_generic_list_projection.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program =
            MirProgram::from_checked_program(&checked).expect("generic List projection MIR");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListProjection {
                index_value: 0,
                ..
            }
        ));
        let results = verify_program(&program, "generic-list-projection-source-hash".into())
            .expect("verify generic List projection MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == "function:main")
            .expect("List projection main verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::Proven,
            "{}",
            result.message
        );
        assert!(result
            .message
            .contains("canonical MIR ensures contract proven"));
    }

    #[test]
    fn verifier_consumes_owned_string_generic_identity_glue_contract() {
        let source =
            include_str!("../../tests/fixtures/mir_native_generic_owned_string_identity.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program =
            MirProgram::from_checked_program(&checked).expect("owned String generic identity MIR");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("owned String identity instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedStringIdentity
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference owned String generic identity execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));

        let results = verify_program(&program, "generic-owned-string-source-hash".into())
            .expect("verify owned String generic identity MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("owned String generic identity verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
    }

    #[test]
    fn verifier_consumes_materialized_generic_variant_identity_call() {
        let source = r#"
            func identity<T>(value: T) -> T { value }

            func checked() -> i32 {
                ensures: result == 18
                let option_value: Option<i32> = Some(7)
                let result_value: Result<i32, i32> = Ok(11)
                let option_roundtrip = identity(option_value)
                let result_roundtrip = identity(result_value)
                if option_roundtrip.is_some() {
                    if result_roundtrip.is_ok() { 18 } else { 0 }
                } else {
                    0
                }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("generic variant MIR");
        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("checked"))
            .cloned()
            .expect("checked MIR function");
        let receipts = program
            .functions()
            .get(&owner)
            .expect("checked function")
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 2);
        assert!(receipts
            .iter()
            .any(|receipt| receipt.nominal.as_str() == "builtin:type:Option"));
        assert!(receipts
            .iter()
            .any(|receipt| receipt.nominal.as_str() == "builtin:type:Result"));

        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic variant call execution");
        assert_eq!(reference, MirRuntimeValue::Int(18));
        let results = verify_program(&program, "generic-variant-identity-source-hash".into())
            .expect("verify generic variant MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("generic variant contract verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
    }

    #[test]
    fn verifier_merges_materialized_generic_variant_identity_branch_paths() {
        let source =
            include_str!("../../tests/fixtures/mir_native_generic_variant_identity_multipath.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program =
            MirProgram::from_checked_program(&checked).expect("generic identity branch MIR");
        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("checked"))
            .cloned()
            .expect("checked MIR function");
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic branch identity execution");
        assert_eq!(reference, MirRuntimeValue::Int(7));

        let results = verify_program(
            &program,
            "generic-variant-identity-multipath-source-hash".into(),
        )
        .expect("verify generic branch identity MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("generic branch identity verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
    }

    #[test]
    fn verifier_rejects_materialized_generic_scalar_identity_branch_merge() {
        let source = r#"
            func identity<T>(value: T) -> T {
                if true { value } else { value }
            }

            func checked() -> i32 {
                ensures: result == 41
                identity(41)
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program =
            MirProgram::from_checked_program(&checked).expect("generic scalar branch identity MIR");
        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("checked"))
            .cloned()
            .expect("checked MIR function");
        let results = verify_program(
            &program,
            "generic-scalar-identity-multipath-source-hash".into(),
        )
        .expect("verify should classify unsupported scalar path merge");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("generic scalar branch verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(result
            .message
            .contains("only admits flat Copy Option/Result"));
    }

    #[test]
    fn verifier_consumes_direct_flat_copy_variant_call_receipt() {
        let source = r#"
            func make() -> Option<i32> { Some(7) }

            func checked() -> i32 {
                ensures: result == 4
                let value = make()
                if value.is_some() { 4 } else { 0 }
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
            .find(|owner| owner.0.ends_with("checked"))
            .cloned()
            .expect("checked MIR function");
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference direct variant call execution");
        assert_eq!(reference, MirRuntimeValue::Int(4));
        let call = program
            .functions()
            .get(&owner)
            .expect("checked function")
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("direct variant call receipt");
        assert_eq!(call.nominal.as_str(), "builtin:type:Option");

        let results =
            verify_program(&program, "direct-variant-call-source-hash".into()).expect("verify MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("direct variant call verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
    }

    #[test]
    fn verifier_merges_total_direct_flat_copy_variant_call_paths() {
        let source = include_str!("../../tests/fixtures/mir_native_variant_call_multipath.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("checked"))
            .cloned()
            .expect("checked MIR function");
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[MirRuntimeValue::Bool(true)])
            .expect("reference multipath call execution");
        assert_eq!(reference, MirRuntimeValue::Int(4));

        let results = verify_program(&program, "direct-variant-multipath-source-hash".into())
            .expect("verify total direct variant call");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("multipath direct variant verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
    }

    #[test]
    fn verifier_rejects_switch_direct_variant_call_path_merge() {
        let source = r#"
            func choose(code: i32) -> Option<i32> {
                match code {
                    0 => Some(7),
                    _ => None
                }
            }

            func checked(code: i32) -> i32 {
                ensures: result >= 0
                let value = choose(code)
                if value.is_some() { 4 } else { 0 }
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
            .find(|owner| owner.0.ends_with("checked"))
            .cloned()
            .expect("checked MIR function");
        let results = verify_program(&program, "direct-variant-switch-merge-source-hash".into())
            .expect("verify should classify unsupported path merge");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("switch merge verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(result.message.contains("only admits Goto/Branch CFG"));
    }

    #[test]
    fn verifier_rejects_tampered_generic_identity_target_shape() {
        let source = r#"
            func identity<T>(value: T) -> T { value }

            func checked() -> i32 {
                ensures: result == 41
                identity(41)
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let canonical = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let instance = canonical
            .instances()
            .values()
            .next()
            .expect("identity instance");
        let mut target = canonical
            .functions()
            .get(&instance.function)
            .cloned()
            .expect("identity target");
        let entry = target.entry.clone();
        target
            .blocks
            .get_mut(&entry)
            .expect("identity entry")
            .instructions
            .clear();
        let mut functions = canonical.functions().clone();
        functions.insert(instance.function.clone(), target);
        let error = MirProgram::with_type_catalog_and_instances(
            functions,
            canonical.type_catalog().clone(),
            canonical.instances().clone(),
        )
        .expect_err("tampered generic target must fail before verifier");
        assert!(
            error.iter().any(|error| {
                error.subject == instance.id.to_string()
                    && error.message.contains("exactly one Clone")
            }),
            "{error:?}"
        );
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
    fn verifier_and_reference_oracle_preserve_immutable_scalar_borrow() {
        let source = r#"
            func read(value: i32) -> i32 {
                ensures: result == value
                *(&value)
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
            .find(|owner| owner.0.ends_with("read"))
            .cloned()
            .expect("read MIR function");

        let reference_value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[MirRuntimeValue::Int(41)])
            .expect("reference borrow execution");
        assert_eq!(reference_value, MirRuntimeValue::Int(41));

        let results = verify_program(&program, "borrow-source-hash".into()).expect("verify MIR");
        let result = results
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("borrow contract verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        assert!(program
            .functions()
            .get(&owner)
            .expect("borrow function")
            .canonical_text()
            .contains("borrow"));
    }

    #[test]
    fn verifier_and_reference_oracle_materialize_scalar_set_contract() {
        let source = r#"
            func size_of(values: Set<i32>) -> i32 {
                ensures: result >= 0
                values.size()
            }

            func normalize() -> i32 {
                ensures: result == 2
                let values: Set<i32> = {1, 2, 1}
                let inserted = values.insert(3)
                let removed = inserted.remove(1)
                removed.size()
            }

            func list_view(values: Set<i32>) -> List<i32> {
                ensures: true
                values.to_list()
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let size_owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("size_of"))
            .cloned()
            .expect("size_of MIR function");
        let normalize_owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("normalize"))
            .cloned()
            .expect("normalize MIR function");
        let list_view_owner = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("list_view"))
            .cloned()
            .expect("list_view MIR function");

        let size = MirReferenceInterpreter::new(&program)
            .execute(
                &size_owner,
                &[MirRuntimeValue::Set(vec![
                    MirRuntimeValue::Int(1),
                    MirRuntimeValue::Int(2),
                    MirRuntimeValue::Int(3),
                ])],
            )
            .expect("reference Set parameter execution");
        assert_eq!(size, MirRuntimeValue::Int(3));
        let normalized = MirReferenceInterpreter::new(&program)
            .execute(&normalize_owner, &[])
            .expect("reference Set construction execution");
        assert_eq!(normalized, MirRuntimeValue::Int(2));
        let list = MirReferenceInterpreter::new(&program)
            .execute(
                &list_view_owner,
                &[MirRuntimeValue::Set(vec![
                    MirRuntimeValue::Int(3),
                    MirRuntimeValue::Int(1),
                    MirRuntimeValue::Int(2),
                ])],
            )
            .expect("reference Set.to_list execution");
        assert_eq!(
            list,
            MirRuntimeValue::List(vec![
                MirRuntimeValue::Int(1),
                MirRuntimeValue::Int(2),
                MirRuntimeValue::Int(3),
            ])
        );

        let results = verify_program(&program, "set-source-hash".into()).expect("verify MIR");
        for owner in [size_owner, normalize_owner, list_view_owner] {
            let result = results
                .iter()
                .find(|result| result.func_name == owner.0)
                .expect("Set contract verification result");
            assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
        }
    }

    #[test]
    fn verifier_gate_rejects_mutable_borrow_before_symbolic_consumption() {
        let source = "func main() -> i32 { let value = 41; (&value); 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let canonical = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut function = canonical.functions().get(&owner).cloned().expect("main");
        let borrow = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find_map(|instruction| match &mut instruction.kind {
                crate::core::mir::MirInstructionKind::Borrow { mutable, .. } => {
                    *mutable = true;
                    Some(instruction.id.clone())
                }
                _ => None,
            })
            .expect("borrow instruction");
        let errors = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            canonical.type_catalog().clone(),
        )
        .expect_err("mutable borrow must fail before verifier consumption");
        assert!(errors.iter().any(|error| {
            error.subject == borrow.to_string() && error.message.contains("mutable Borrow")
        }));
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
    fn verifier_proves_non_copy_option_string_switch_move() {
        let source = r#"
            func consume(value: Option<string>) -> i32 {
                ensures: result >= 0
                match value {
                    Some(_) => 41,
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
            .expect("verifier should prove the admitted variant island");
        let result = results
            .iter()
            .find(|result| result.func_name.ends_with("consume"))
            .expect("consume verification result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
    }

    #[test]
    fn verifier_rejects_non_copy_option_string_switch_move_default_directly() {
        let source = r#"
            func consume(value: Option<string>) -> i32 {
                ensures: result >= 0
                match value {
                    Some(_) => 41,
                    _ => 0
                }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR gate");
        let results = verify_program(
            &program,
            "non-copy-option-string-default-source-hash".into(),
        )
        .expect("verifier should return a classified result");
        let result = results
            .iter()
            .find(|result| result.func_name.ends_with("consume"))
            .expect("consume verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(result.message.contains("explicit variant arms"));
    }

    #[test]
    fn verifier_rejects_non_copy_nested_variant_switch_move() {
        let source = r#"
            func consume(value: Option<(string, i32)>) -> i32 {
                ensures: result >= 0
                match value {
                    Some(_) => 41,
                    None => 0
                }
            }

            func main() -> i32 { 0 }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR gate");
        let results = verify_program(&program, "nested-non-copy-variant-source-hash".into())
            .expect("verifier should return a classified result");
        let result = results
            .iter()
            .find(|result| result.func_name.ends_with("consume"))
            .expect("consume verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(result.message.contains("Copy/no-op aggregate contract"));
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
