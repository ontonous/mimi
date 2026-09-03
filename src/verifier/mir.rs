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

    // The owned-String verifier slice proves only arithmetic contracts whose
    // observable result is a Copy scalar.  String payloads stay opaque in the
    // symbolic domain; admitting an aggregate or owned result here would
    // silently turn this proof into a new ABI/ownership slice.  The check is
    // intentionally driven by the canonical value catalog, never by a
    // surface type or backend-specific representation.
    let has_owned_string_value = function
        .values
        .values()
        .any(|value| catalog.validate_owned_string(&value.ty).is_ok());
    if has_owned_string_value && catalog.validate_copy_scalar(&function.result).is_err() {
        return Err(
            "canonical MIR verifier owned String slice requires a Copy scalar result".into(),
        );
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
    let linear_record = descriptor.ownership == MirOwnership::Linear
        && matches!(&descriptor.layout, MirLayout::Record { .. })
        && descriptor.abi == MirAbiClass::Aggregate
        && descriptor.glue.move_out == crate::core::mir::types::MirGlueKind::Aggregate
        && descriptor.glue.clone == crate::core::mir::types::MirGlueKind::Aggregate
        && descriptor.glue.drop == crate::core::mir::types::MirGlueKind::Aggregate
        && descriptor.drop_plan.is_some();
    let move_owned_option_string = catalog.validate_option_string_variant(ty).is_ok();
    let move_owned_tuple = matches!(descriptor.layout, MirLayout::Tuple(_))
        && catalog.validate_recursive_tuple_abi(ty).is_ok();
    if (descriptor.ownership != MirOwnership::Copy
        && !linear_record
        && !move_owned_option_string
        && !move_owned_tuple)
        || (!linear_record
            && !move_owned_option_string
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
        catalog.validate_option_string_variant(&scrutinee_ty)?;
        catalog.validate_switch_move(&scrutinee_ty, arms)?;
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
                    let value = payload.get(&field.id).cloned().ok_or_else(|| {
                        format!(
                            "canonical MIR verifier switch payload field '{}' is absent",
                            field.id.0
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
        MirInstructionKind::ConstructList { result, elements } => {
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
            catalog.validate_list_construct(&result_ty, &element_types)?;
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
            let receipt = list_operation_contract
                .as_ref()
                .ok_or_else(|| "MIR List operation has no canonical receipt".to_string())?;
            catalog.validate_list_operation_receipt(&result_ty, &list_ty, *operation, receipt)?;
            let SymbolicValue::List { length } = state
                .values
                .get(list)
                .cloned()
                .ok_or_else(|| format!("MIR List receiver '{}' is not defined", list))?
            else {
                return Err("MIR List operation receiver is not a symbolic List".into());
            };
            let fits_i32 = length.le(Int::from_i64(i32::MAX as i64));
            add_definedness(state, fits_i32, "E0802")?;
            let value = match operation {
                MirListOperation::Len => SymbolicValue::Int(length),
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
        MirInstructionKind::MoveProject { .. } => {
            return Err("MIR instruction is outside scalar verifier contract".into())
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
            catalog.validate_option_string_variant(&result_ty)?;
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
        } => eval_materialized_call(
            function,
            program,
            catalog,
            state,
            result,
            callee,
            type_arguments,
            arguments,
        )?,
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
) -> Result<(), String> {
    let crate::core::ir::ResolvedCallee::Function(target_owner) = callee else {
        return Err("MIR verifier call callee is not a canonical function instance".into());
    };
    let instance = program
        .instances()
        .values()
        .find(|instance| instance.function == *target_owner)
        .ok_or_else(|| {
            format!(
                "MIR verifier call target '{}' is absent from the instance table",
                target_owner.0
            )
        })?;
    if instance.arguments != type_arguments {
        return Err(format!(
            "MIR verifier call target '{}' disagrees with its instance arguments",
            target_owner.0
        ));
    }
    match instance.contract {
        crate::core::mir::MirGenericInstanceContract::ScalarIdentity => {
            eval_materialized_identity_call(
                function,
                program,
                catalog,
                state,
                result,
                callee,
                type_arguments,
                arguments,
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
                operation,
            )
        }
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

/// Symbolically consume the scalar generic identity call admitted by this
/// slice.
///
/// The verifier does not infer a callee body from a template name.  It first
/// requires the call to name an instance in the canonical instance table,
/// then checks that the executable target still has the exact specialized
/// `Clone(parameter) -> Return` shape produced by MIR lowering.  This keeps
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
) -> Result<(), String> {
    let crate::core::ir::ResolvedCallee::Function(target_owner) = callee else {
        return Err("MIR verifier generic call callee is not a canonical function instance".into());
    };
    if type_arguments.len() != 1 || arguments.len() != 1 {
        return Err("MIR verifier only admits one-argument scalar generic identity calls".into());
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
    catalog.validate_scalar_generic_arguments(type_arguments)?;

    let target = program
        .functions()
        .get(target_owner)
        .ok_or_else(|| format!("MIR verifier generic target '{}' is absent", target_owner.0))?;
    let [target_parameter] = target.parameters.as_slice() else {
        return Err("MIR verifier generic identity target must have one parameter".into());
    };
    let concrete = type_arguments
        .first()
        .expect("validated one generic type argument");
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
    let block = target
        .blocks
        .get(&target.entry)
        .filter(|_| target.blocks.len() == 1)
        .ok_or_else(|| {
            "MIR verifier generic identity target must have one entry block".to_string()
        })?;
    let [instruction] = block.instructions.as_slice() else {
        return Err("MIR verifier generic identity target must contain exactly one Clone".into());
    };
    let MirInstructionKind::Clone {
        result: cloned_value,
        source,
    } = &instruction.kind
    else {
        return Err("MIR verifier generic identity target must use Clone".into());
    };
    if source != target_parameter
        || !target
            .values
            .get(cloned_value)
            .is_some_and(|value| value.ty == *concrete)
        || !matches!(
            &block.terminator,
            MirTerminator::Return { value: Some(value) } if value == cloned_value
        )
    {
        return Err("MIR verifier generic identity target must return its cloned parameter".into());
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
    value_scalar_kind(function, catalog, argument)?;
    if !symbolic_matches_type(catalog, concrete, &symbolic) {
        return Err("MIR verifier generic call argument has the wrong scalar shape".into());
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
    ensure_result_shape(function, catalog, result, &symbolic)?;
    state.values.insert(result.clone(), symbolic);
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
    fn verifier_rejects_owned_string_result_without_fallback() {
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
            .expect("verifier should classify unsupported owned result");
        let result = results
            .iter()
            .find(|result| result.func_name.ends_with("echo"))
            .expect("echo verification result");
        assert_eq!(
            result.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(result
            .message
            .contains("owned String slice requires a Copy scalar result"));
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
