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
    MirContractBinaryOp, MirContractExpr, MirContractKind, MirContractUnaryOp, MirFunction,
    MirInstructionKind, MirTerminator, MirValueId,
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
        let kind = value_scalar_kind(function, catalog, parameter)?;
        let name = format!("mir.value.{}", parameter.as_str());
        let value = match kind {
            ScalarKind::Int { bits } => {
                let symbol = Int::new_const(name);
                state.constraints.push(int_range_constraint(&symbol, bits));
                SymbolicValue::Int(symbol)
            }
            ScalarKind::Bool => SymbolicValue::Bool(Bool::new_const(name)),
        };
        state.values.insert(parameter.clone(), value);
    }
    // Keep the session argument in the constructor signature so all future
    // canonical initialization constraints have one explicit proof boundary.
    let _ = session;
    Ok(state)
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
        MirTerminator::Switch { .. }
        | MirTerminator::SwitchMove { .. }
        | MirTerminator::Fault { .. } => {
            return Err(
                "canonical MIR verifier currently supports only scalar Goto/Branch CFG".into(),
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
            if !place.projections.is_empty() {
                return Err("MIR verifier does not admit projected scalar loads".into());
            }
            let source = MirValueId::new(format!("local:{}", place.base.0 .0))
                .map_err(|error| error.to_string())?;
            let value = state
                .values
                .get(&source)
                .cloned()
                .ok_or_else(|| format!("MIR load source '{}' is not defined", source))?;
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
            ensure_result_kind(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Drop { value } => {
            let _ = value_scalar_kind(function, catalog, value)?;
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
            ensure_result_kind(function, catalog, result, &output)?;
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
            ensure_result_kind(function, catalog, result, &output)?;
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
            ensure_result_kind(function, catalog, result, &output)?;
            state.values.insert(result.clone(), output);
        }
        MirInstructionKind::Convert { result, source } => {
            let value = state
                .values
                .get(source)
                .cloned()
                .ok_or_else(|| format!("MIR conversion source '{}' is not defined", source))?;
            ensure_result_kind(function, catalog, result, &value)?;
            state.values.insert(result.clone(), value);
        }
        MirInstructionKind::Nop => {}
        MirInstructionKind::Project { .. }
        | MirInstructionKind::MoveProject { .. }
        | MirInstructionKind::Borrow { .. }
        | MirInstructionKind::EndBorrow { .. }
        | MirInstructionKind::Construct { .. }
        | MirInstructionKind::ConstructVariant { .. }
        | MirInstructionKind::ConstructVariantMove { .. }
        | MirInstructionKind::UpdateRecord { .. }
        | MirInstructionKind::Call { .. } => {
            return Err("MIR instruction is outside scalar verifier contract".into())
        }
    }
    Ok(())
}

fn ensure_result_kind(
    function: &MirFunction,
    catalog: &crate::core::mir::types::MirTypeCatalog,
    result: &MirValueId,
    value: &SymbolicValue,
) -> Result<(), String> {
    let expected = value_scalar_kind(function, catalog, result)?;
    let actual = match value {
        SymbolicValue::Int(_) => ScalarKind::Int { bits: 0 },
        SymbolicValue::Bool(_) => ScalarKind::Bool,
    };
    match (expected, actual) {
        (ScalarKind::Int { .. }, ScalarKind::Int { .. }) | (ScalarKind::Bool, ScalarKind::Bool) => {
            Ok(())
        }
        _ => Err(format!(
            "MIR result '{}' disagrees with symbolic scalar kind",
            result
        )),
    }
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
        SymbolicValue::Int(_) => Err(format!("{context} is not boolean")),
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
}
