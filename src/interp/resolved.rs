//! Direct execution of checker-owned typed bodies.
//!
//! This module is the replacement execution core for the surface-AST
//! interpreter. It deliberately accepts only `CheckedProgram` identities and
//! `ResolvedBody` nodes; unsupported typed constructs fail closed instead of
//! consulting the compatibility surface program.

use super::{is_truthy, ops, values_equal, InterpError, Value};
use crate::core::ir::{ResolvedBinaryOp, ResolvedFStringPart, ResolvedValueProjection};
use crate::core::{
    CheckedConversion, CheckedConversionKind, CheckedProgram, NodeId, ResolvedBlock, ResolvedBody,
    ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedIndex, ResolvedLiteral,
    ResolvedLocalId, ResolvedPattern, ResolvedPatternKind, ResolvedPlace, ResolvedProjection,
    ResolvedStmt, ResolvedStmtKind, ResolvedType,
};
use std::collections::BTreeMap;

const MAX_TYPED_CALL_DEPTH: usize = 1024;

pub(crate) struct ResolvedInterpreter<'a> {
    program: &'a CheckedProgram,
    state: ExecutionState,
}

#[derive(Default)]
struct ExecutionState {
    frames: Vec<Frame>,
    call_depth: usize,
}

struct Frame {
    owner: NodeId,
    values: BTreeMap<ResolvedLocalId, Value>,
    signal: Option<ControlSignal>,
}

enum ControlSignal {
    Return(Value),
    Break(Option<Value>),
    Continue,
}

#[derive(Clone)]
enum RuntimeProjection {
    Field(String),
    Tuple(usize),
    Index(usize),
    Dereference,
}

impl<'a> ResolvedInterpreter<'a> {
    pub(crate) fn new(program: &'a CheckedProgram) -> Self {
        Self {
            program,
            state: ExecutionState::default(),
        }
    }

    pub(crate) fn run_main(&mut self) -> Result<Value, InterpError> {
        let main = self
            .program
            .function("main")
            .ok_or_else(|| InterpError::new("no resolved main() callable found"))?;
        execute_call(self.program, &mut self.state, &main.node_id, Vec::new())
    }

    pub(crate) fn call(
        &mut self,
        owner: &NodeId,
        arguments: Vec<Value>,
    ) -> Result<Value, InterpError> {
        execute_call(self.program, &mut self.state, owner, arguments)
    }
}

fn execute_call(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    owner: &NodeId,
    arguments: Vec<Value>,
) -> Result<Value, InterpError> {
    if state.call_depth >= MAX_TYPED_CALL_DEPTH {
        return Err(InterpError::new(
            "typed interpreter recursion limit exceeded",
        ));
    }
    let body = program
        .resolved_body(owner)
        .ok_or_else(|| unsupported(owner, "callable has no ResolvedBody"))?;
    let signature = program
        .resolved_signature(owner)
        .ok_or_else(|| unsupported(owner, "callable has no ResolvedSignature"))?;
    if body.parameters.len() != signature.parameters.len() {
        return Err(unsupported(
            owner,
            "body parameter identities disagree with the canonical signature",
        ));
    }
    if arguments.len() > body.parameters.len() {
        return Err(InterpError::wrong_arg_count(format!(
            "resolved callable '{}' expects {} arguments, got {}",
            owner.0,
            body.parameters.len(),
            arguments.len()
        )));
    }

    let mut values = BTreeMap::new();
    for (local, value) in body.parameters.iter().zip(arguments) {
        values.insert(local.clone(), value);
    }
    state.frames.push(Frame {
        owner: owner.clone(),
        values,
        signal: None,
    });
    state.call_depth += 1;

    let result = (|| {
        for index in state
            .frames
            .last()
            .expect("typed frame installed")
            .values
            .len()..body.parameters.len()
        {
            let parameter = &signature.parameters[index];
            let default = body.default_values.get(&parameter.id).ok_or_else(|| {
                InterpError::wrong_arg_count(format!(
                    "resolved callable '{}' is missing argument '{}'",
                    owner.0, parameter.name
                ))
            })?;
            let value = eval_expr(program, state, body, default)?;
            current_frame(state)?
                .values
                .insert(body.parameters[index].clone(), value);
        }

        let value = eval_block(program, state, body, &body.root)?;
        match current_frame(state)?.signal.take() {
            Some(ControlSignal::Return(value)) => Ok(value),
            Some(ControlSignal::Break(_)) | Some(ControlSignal::Continue) => {
                Err(unsupported(owner, "loop control escaped the callable root"))
            }
            None => Ok(value),
        }
    })();

    state.call_depth -= 1;
    state.frames.pop();
    result.map_err(|error| error.in_func(owner.0.clone()))
}

fn eval_block(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    block: &ResolvedBlock,
) -> Result<Value, InterpError> {
    for statement in &block.statements {
        eval_stmt(program, state, body, statement)?;
        if current_frame(state)?.signal.is_some() {
            return Ok(Value::Unit);
        }
    }
    block
        .result
        .as_deref()
        .map(|result| eval_expr(program, state, body, result))
        .unwrap_or(Ok(Value::Unit))
}

fn eval_stmt(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    statement: &ResolvedStmt,
) -> Result<(), InterpError> {
    match &statement.kind {
        ResolvedStmtKind::Bind {
            pattern,
            initializer,
        } => {
            let value = initializer
                .as_ref()
                .map(|value| eval_expr(program, state, body, value))
                .transpose()?
                .unwrap_or(Value::Unit);
            bind_pattern(state, pattern, value)?;
        }
        ResolvedStmtKind::Assign {
            target,
            value,
            conversion,
        } => {
            let value = eval_expr(program, state, body, value)?;
            let value = apply_conversion(program, conversion, value)?;
            write_place(program, state, body, target, value)?;
        }
        ResolvedStmtKind::Return { value, conversion } => {
            let value = match (value, conversion) {
                (Some(value), Some(conversion)) => {
                    let value = eval_expr(program, state, body, value)?;
                    apply_conversion(program, conversion, value)?
                }
                (None, None) => Value::Unit,
                _ => return Err(unsupported(&statement.node_id, "invalid typed return")),
            };
            current_frame(state)?.signal = Some(ControlSignal::Return(value));
        }
        ResolvedStmtKind::Break(value) => {
            let value = value
                .as_ref()
                .map(|value| eval_expr(program, state, body, value))
                .transpose()?;
            current_frame(state)?.signal = Some(ControlSignal::Break(value));
        }
        ResolvedStmtKind::Continue => {
            current_frame(state)?.signal = Some(ControlSignal::Continue);
        }
        ResolvedStmtKind::Expr(expression) => {
            eval_expr(program, state, body, expression)?;
        }
        ResolvedStmtKind::While {
            condition,
            body: loop_body,
        } => loop {
            let condition = eval_expr(program, state, body, condition)?;
            if !is_truthy(&condition) {
                break;
            }
            eval_block(program, state, body, loop_body)?;
            if consume_loop_signal(state)? {
                break;
            }
        },
        ResolvedStmtKind::WhileLet {
            pattern,
            initializer,
            body: loop_body,
        } => loop {
            let initializer = eval_expr(program, state, body, initializer)?;
            if !try_bind_pattern(state, pattern, initializer)? {
                break;
            }
            eval_block(program, state, body, loop_body)?;
            if consume_loop_signal(state)? {
                break;
            }
        },
        ResolvedStmtKind::Loop(loop_body) => loop {
            eval_block(program, state, body, loop_body)?;
            if consume_loop_signal(state)? {
                break;
            }
        },
        ResolvedStmtKind::For {
            pattern,
            iterable,
            body: loop_body,
        } => {
            let iterable = eval_expr(program, state, body, iterable)?;
            for value in iterable_values(&statement.node_id, iterable)? {
                bind_pattern(state, pattern, value)?;
                eval_block(program, state, body, loop_body)?;
                if consume_loop_signal(state)? {
                    break;
                }
            }
        }
        ResolvedStmtKind::Drop(places) => {
            for place in places {
                if place.projections.is_empty() {
                    current_frame(state)?.values.remove(&place.base);
                } else {
                    write_place(program, state, body, place, Value::Unit)?;
                }
            }
        }
        ResolvedStmtKind::Contract { kind, condition } => {
            let condition = eval_expr(program, state, body, condition)?;
            if !is_truthy(&condition) {
                return Err(InterpError::contract_violation(format!(
                    "typed {:?} contract failed at '{}'",
                    kind, statement.node_id.0
                )));
            }
        }
        ResolvedStmtKind::Math(expressions) => {
            for expression in expressions {
                eval_expr(program, state, body, expression)?;
            }
        }
        ResolvedStmtKind::Scope {
            body: scope_body, ..
        } => {
            eval_block(program, state, body, scope_body)?;
        }
        ResolvedStmtKind::NestedCallable(_) => {}
        ResolvedStmtKind::Delegate { .. } | ResolvedStmtKind::Pinned { .. } => {
            return Err(unsupported(
                &statement.node_id,
                "delegate/pinned execution is outside the typed scalar subset",
            ));
        }
    }
    Ok(())
}

fn eval_expr(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    expression: &ResolvedExpr,
) -> Result<Value, InterpError> {
    match &expression.kind {
        ResolvedExprKind::Literal(literal) => Ok(literal_value(literal)),
        ResolvedExprKind::FString(parts) => {
            let mut output = String::new();
            for part in parts {
                match part {
                    ResolvedFStringPart::Text(text) => output.push_str(text),
                    ResolvedFStringPart::Interpolation(value) => {
                        output.push_str(&eval_expr(program, state, body, value)?.to_string());
                    }
                }
            }
            Ok(Value::String(output))
        }
        ResolvedExprKind::Load(place) => read_place(program, state, body, place),
        ResolvedExprKind::Binary { op, left, right } => {
            let left = eval_expr(program, state, body, left)?;
            if *op == ResolvedBinaryOp::LogicalAnd && !is_truthy(&left) {
                return Ok(Value::Bool(false));
            }
            if *op == ResolvedBinaryOp::LogicalOr && is_truthy(&left) {
                return Ok(Value::Bool(true));
            }
            let right = eval_expr(program, state, body, right)?;
            ops::apply_binary(*op, left, right)
        }
        ResolvedExprKind::Unary { op, operand } => {
            let operand = eval_expr(program, state, body, operand)?;
            ops::apply_unary(*op, operand)
        }
        ResolvedExprKind::Call(call) => {
            let mut arguments = Vec::with_capacity(call.arguments.len());
            for argument in &call.arguments {
                let value = eval_expr(program, state, body, &argument.value)?;
                arguments.push(apply_conversion(program, &argument.conversion, value)?);
            }
            match &call.callee {
                ResolvedCallee::Function(owner) => execute_call(program, state, owner, arguments),
                ResolvedCallee::Constructor(owner) => {
                    if program.type_defs().get(owner).is_some_and(|definition| {
                        definition.kind == crate::core::ResolvedTypeKind::Newtype
                    }) {
                        let [value] = arguments.as_slice() else {
                            return Err(unsupported(
                                &expression.node_id,
                                "newtype constructor arity is not one",
                            ));
                        };
                        Ok(Value::Newtype(owner.0.clone(), Box::new(value.clone())))
                    } else {
                        Ok(Value::Variant(owner.0.clone(), arguments))
                    }
                }
                _ => Err(unsupported(
                    &expression.node_id,
                    "callee is not yet in the typed scalar execution subset",
                )),
            }
        }
        ResolvedExprKind::Tuple(values) => {
            eval_values(program, state, body, values).map(Value::Tuple)
        }
        ResolvedExprKind::List(values) => {
            eval_values(program, state, body, values).map(Value::List)
        }
        ResolvedExprKind::Set(values) => {
            let mut set = Vec::new();
            for value in eval_values(program, state, body, values)? {
                if !set.iter().any(|existing| values_equal(existing, &value)) {
                    set.push(value);
                }
            }
            Ok(Value::Set(set))
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block) => eval_block(program, state, body, block),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            let condition = eval_expr(program, state, body, condition)?;
            if is_truthy(&condition) {
                eval_block(program, state, body, then_block)
            } else {
                eval_block(program, state, body, else_block)
            }
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            let value = eval_expr(program, state, body, scrutinee)?;
            for arm in arms {
                let previous = current_frame(state)?.values.clone();
                if !try_bind_pattern(state, &arm.pattern, value.clone())? {
                    continue;
                }
                if let Some(guard) = &arm.guard {
                    let guard = eval_expr(program, state, body, guard)?;
                    if !is_truthy(&guard) {
                        current_frame(state)?.values = previous;
                        continue;
                    }
                }
                return eval_expr(program, state, body, &arm.body);
            }
            Err(InterpError::non_exhaustive_match(
                "resolved match has no matching arm",
            ))
        }
        ResolvedExprKind::Range { start, end } => {
            match (
                eval_expr(program, state, body, start)?,
                eval_expr(program, state, body, end)?,
            ) {
                (Value::Int(start), Value::Int(end)) => Ok(Value::Range { start, end }),
                _ => Err(unsupported(
                    &expression.node_id,
                    "range bounds are not integers",
                )),
            }
        }
        ResolvedExprKind::Cast { value, conversion } => {
            let value = eval_expr(program, state, body, value)?;
            apply_conversion(program, conversion, value)
        }
        ResolvedExprKind::Project { value, projection } => {
            let value = eval_expr(program, state, body, value)?;
            read_projection(program, state, body, value, projection)
        }
        ResolvedExprKind::Old(value) => eval_expr(program, state, body, value),
        ResolvedExprKind::Record { .. }
        | ResolvedExprKind::Map(_)
        | ResolvedExprKind::Comprehension { .. }
        | ResolvedExprKind::OptionalChain { .. }
        | ResolvedExprKind::TypeOf(_)
        | ResolvedExprKind::Try { .. }
        | ResolvedExprKind::Slice { .. }
        | ResolvedExprKind::Spawn(_)
        | ResolvedExprKind::Await(_)
        | ResolvedExprKind::Lambda(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::Quote(_)
        | ResolvedExprKind::TypeValue(_) => Err(unsupported(
            &expression.node_id,
            "expression is outside the typed scalar execution subset",
        )),
    }
}

fn eval_values(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    values: &[ResolvedExpr],
) -> Result<Vec<Value>, InterpError> {
    values
        .iter()
        .map(|value| eval_expr(program, state, body, value))
        .collect()
}

fn bind_pattern(
    state: &mut ExecutionState,
    pattern: &ResolvedPattern,
    value: Value,
) -> Result<(), InterpError> {
    if try_bind_pattern(state, pattern, value)? {
        Ok(())
    } else {
        Err(InterpError::new(format!(
            "resolved pattern '{}' does not match its checked value",
            pattern.node_id.0
        )))
    }
}

fn try_bind_pattern(
    state: &mut ExecutionState,
    pattern: &ResolvedPattern,
    value: Value,
) -> Result<bool, InterpError> {
    let previous = current_frame(state)?.values.clone();
    let matched = match_pattern(state, pattern, value)?;
    if !matched {
        current_frame(state)?.values = previous;
    }
    Ok(matched)
}

fn match_pattern(
    state: &mut ExecutionState,
    pattern: &ResolvedPattern,
    value: Value,
) -> Result<bool, InterpError> {
    match &pattern.kind {
        ResolvedPatternKind::Wildcard => Ok(true),
        ResolvedPatternKind::Binding { local, .. } => {
            current_frame(state)?.values.insert(local.clone(), value);
            Ok(true)
        }
        ResolvedPatternKind::Literal(literal) => Ok(values_equal(&literal_value(literal), &value)),
        ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => {
            let values = match value {
                Value::Tuple(values) | Value::List(values) | Value::Array(values) => values,
                _ => return Ok(false),
            };
            match_pattern_list(state, patterns, values)
        }
        ResolvedPatternKind::Constructor { variant, fields } => {
            let values = match value {
                Value::Variant(identity, values) if identity == variant.0 => values,
                Value::Newtype(identity, value) if identity == variant.0 => vec![*value],
                _ => return Ok(false),
            };
            let patterns = fields
                .iter()
                .map(|(_, pattern)| pattern.clone())
                .collect::<Vec<_>>();
            match_pattern_list(state, &patterns, values)
        }
        ResolvedPatternKind::Slice { prefix, rest } => {
            let values = match value {
                Value::List(values) | Value::Array(values) => values,
                _ => return Ok(false),
            };
            if values.len() < prefix.len() || (rest.is_none() && values.len() != prefix.len()) {
                return Ok(false);
            }
            for (pattern, value) in prefix.iter().zip(values.iter().cloned()) {
                if !match_pattern(state, pattern, value)? {
                    return Ok(false);
                }
            }
            if let Some(rest) = rest {
                match_pattern(state, rest, Value::List(values[prefix.len()..].to_vec()))
            } else {
                Ok(true)
            }
        }
    }
}

fn match_pattern_list(
    state: &mut ExecutionState,
    patterns: &[ResolvedPattern],
    values: Vec<Value>,
) -> Result<bool, InterpError> {
    if patterns.len() != values.len() {
        return Ok(false);
    }
    for (pattern, value) in patterns.iter().zip(values) {
        if !match_pattern(state, pattern, value)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_place(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    place: &ResolvedPlace,
) -> Result<Value, InterpError> {
    let value = current_frame(state)?
        .values
        .get(&place.base)
        .cloned()
        .ok_or_else(|| InterpError::new(format!("unbound resolved local '{}'", place.base.0 .0)))?;
    let projections = evaluate_projections(program, state, body, place)?;
    projections.iter().try_fold(value, project_value)
}

fn write_place(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    place: &ResolvedPlace,
    value: Value,
) -> Result<(), InterpError> {
    let projections = evaluate_projections(program, state, body, place)?;
    let root = current_frame(state)?
        .values
        .get_mut(&place.base)
        .ok_or_else(|| InterpError::new(format!("unbound resolved local '{}'", place.base.0 .0)))?;
    write_projection(root, &projections, value)
}

fn evaluate_projections(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    place: &ResolvedPlace,
) -> Result<Vec<RuntimeProjection>, InterpError> {
    let mut values = Vec::with_capacity(place.projections.len());
    for projection in &place.projections {
        values.push(match projection {
            ResolvedProjection::Field { name, .. } => RuntimeProjection::Field(name.clone()),
            ResolvedProjection::Tuple { index, .. } => RuntimeProjection::Tuple(*index),
            ResolvedProjection::Index { index, .. } => {
                let index = match index {
                    ResolvedIndex::Constant(index) => usize::try_from(*index)
                        .map_err(|_| InterpError::index_out_of_bounds("negative resolved index"))?,
                    ResolvedIndex::Dynamic(node) => {
                        let input = body.place_inputs.get(node).ok_or_else(|| {
                            unsupported(node, "dynamic place index has no typed input")
                        })?;
                        match eval_expr(program, state, body, input)? {
                            Value::Int(index) => usize::try_from(index).map_err(|_| {
                                InterpError::index_out_of_bounds("negative resolved index")
                            })?,
                            _ => return Err(unsupported(node, "dynamic index is not an integer")),
                        }
                    }
                };
                RuntimeProjection::Index(index)
            }
            ResolvedProjection::Deref { .. } => RuntimeProjection::Dereference,
        });
    }
    Ok(values)
}

fn project_value(value: Value, projection: &RuntimeProjection) -> Result<Value, InterpError> {
    match (value, projection) {
        (Value::Record(_, fields), RuntimeProjection::Field(field)) => fields
            .get(field)
            .cloned()
            .ok_or_else(|| InterpError::field_not_found(format!("field '{field}' not found"))),
        (Value::Tuple(values), RuntimeProjection::Tuple(index))
        | (Value::List(values), RuntimeProjection::Index(index))
        | (Value::Array(values), RuntimeProjection::Index(index)) => values
            .get(*index)
            .cloned()
            .ok_or_else(|| InterpError::index_out_of_bounds(format!("index {index}"))),
        (Value::Newtype(_, value), RuntimeProjection::Tuple(0)) => Ok(*value),
        (value, RuntimeProjection::Dereference) => match value {
            Value::Shared(value) | Value::Ref(value) | Value::RefMut(value) => value
                .read()
                .map_err(|_| InterpError::lock_error("poisoned typed reference"))
                .map(|value| value.clone()),
            Value::LocalShared(value) => value
                .lock()
                .map_err(|_| InterpError::lock_error("poisoned typed local reference"))
                .map(|value| value.clone()),
            _ => Err(InterpError::new(
                "resolved dereference target is not reference-like",
            )),
        },
        _ => Err(InterpError::new(
            "resolved projection does not match runtime value",
        )),
    }
}

fn write_projection(
    target: &mut Value,
    projections: &[RuntimeProjection],
    value: Value,
) -> Result<(), InterpError> {
    let Some((projection, rest)) = projections.split_first() else {
        *target = value;
        return Ok(());
    };
    let child = match (target, projection) {
        (Value::Record(_, fields), RuntimeProjection::Field(field)) => fields
            .get_mut(field)
            .ok_or_else(|| InterpError::field_not_found(format!("field '{field}' not found")))?,
        (Value::Tuple(values), RuntimeProjection::Tuple(index))
        | (Value::List(values), RuntimeProjection::Index(index))
        | (Value::Array(values), RuntimeProjection::Index(index)) => values
            .get_mut(*index)
            .ok_or_else(|| InterpError::index_out_of_bounds(format!("index {index}")))?,
        (Value::Newtype(_, child), RuntimeProjection::Tuple(0)) => child,
        _ => {
            return Err(InterpError::new(
                "resolved assignment projection is invalid",
            ))
        }
    };
    write_projection(child, rest, value)
}

fn read_projection(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    value: Value,
    projection: &ResolvedValueProjection,
) -> Result<Value, InterpError> {
    match projection {
        ResolvedValueProjection::Tuple(index) => {
            project_value(value, &RuntimeProjection::Tuple(*index))
        }
        ResolvedValueProjection::Index(index) => {
            let index = match eval_expr(program, state, body, index)? {
                Value::Int(index) => usize::try_from(index)
                    .map_err(|_| InterpError::index_out_of_bounds("negative resolved index"))?,
                _ => {
                    return Err(unsupported(
                        &index.node_id,
                        "rvalue index is not an integer",
                    ))
                }
            };
            project_value(value, &RuntimeProjection::Index(index))
        }
        ResolvedValueProjection::Dereference => {
            project_value(value, &RuntimeProjection::Dereference)
        }
        ResolvedValueProjection::Field(field) => Err(unsupported(
            field,
            "rvalue field projection needs the resolved field display catalog",
        )),
    }
}

fn apply_conversion(
    program: &CheckedProgram,
    conversion: &CheckedConversion,
    value: Value,
) -> Result<Value, InterpError> {
    match conversion.kind {
        CheckedConversionKind::Identity
        | CheckedConversionKind::AliasWrap
        | CheckedConversionKind::AliasUnwrap
        | CheckedConversionKind::TraitUpcast
        | CheckedConversionKind::LifetimeRebind
        | CheckedConversionKind::SliceView => Ok(value),
        CheckedConversionKind::NumericWiden => {
            match (value, program.resolved_types().get(&conversion.to)) {
                (
                    Value::Int(value),
                    Some(ResolvedType::Primitive(crate::core::PrimitiveType::F64)),
                ) => Ok(Value::Float(value as f64)),
                (value, _) => Ok(value),
            }
        }
        CheckedConversionKind::NumericNarrowChecked => match value {
            Value::Float(value)
                if value.is_finite()
                    && value.fract() == 0.0
                    && value >= i64::MIN as f64
                    && value <= i64::MAX as f64 =>
            {
                Ok(Value::Int(value as i64))
            }
            Value::Int(value) => Ok(Value::Int(value)),
            _ => Err(InterpError::type_mismatch(
                "checked numeric narrowing failed at runtime",
            )),
        },
        CheckedConversionKind::NewtypeWrap => {
            let Some(ResolvedType::Newtype { item, .. }) =
                program.resolved_types().get(&conversion.to)
            else {
                return Err(InterpError::type_mismatch(
                    "newtype wrap target is not canonical newtype",
                ));
            };
            Ok(Value::Newtype(item.as_str().to_string(), Box::new(value)))
        }
        CheckedConversionKind::NewtypeUnwrap => match value {
            Value::Newtype(_, value) => Ok(*value),
            _ => Err(InterpError::type_mismatch(
                "newtype unwrap source is not a newtype value",
            )),
        },
        CheckedConversionKind::OwnershipWrap
        | CheckedConversionKind::OwnershipDowngrade
        | CheckedConversionKind::OwnershipRead
        | CheckedConversionKind::DynamicPack
        | CheckedConversionKind::DynamicDowncastChecked => Err(InterpError::new(
            "conversion is outside the typed scalar execution subset",
        )),
    }
}

fn iterable_values(owner: &NodeId, value: Value) -> Result<Vec<Value>, InterpError> {
    match value {
        Value::List(values) | Value::Array(values) | Value::Set(values) => Ok(values),
        Value::Range { start, end } => Ok((start..end).map(Value::Int).collect()),
        _ => Err(unsupported(owner, "resolved for iterable is not finite")),
    }
}

fn consume_loop_signal(state: &mut ExecutionState) -> Result<bool, InterpError> {
    match current_frame(state)?.signal.take() {
        Some(ControlSignal::Break(value)) => {
            let _ = value;
            Ok(true)
        }
        Some(ControlSignal::Continue) => Ok(false),
        Some(signal @ ControlSignal::Return(_)) => {
            current_frame(state)?.signal = Some(signal);
            Ok(true)
        }
        None => Ok(false),
    }
}

fn current_frame(state: &mut ExecutionState) -> Result<&mut Frame, InterpError> {
    state
        .frames
        .last_mut()
        .ok_or_else(|| InterpError::new("typed interpreter has no active frame"))
}

fn literal_value(literal: &ResolvedLiteral) -> Value {
    match literal {
        ResolvedLiteral::Int(value) => Value::Int(*value),
        ResolvedLiteral::FloatBits(bits) => Value::Float(f64::from_bits(*bits)),
        ResolvedLiteral::Bool(value) => Value::Bool(*value),
        ResolvedLiteral::String(value) => Value::String(value.clone()),
        ResolvedLiteral::Unit => Value::Unit,
    }
}

fn unsupported(node: &NodeId, detail: &str) -> InterpError {
    InterpError::new(format!(
        "typed interpreter does not support node '{}': {detail}",
        node.0
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex typed interpreter fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse typed interpreter fixture");
        crate::core::check_program(&file).expect("check typed interpreter fixture")
    }

    #[test]
    fn typed_scalar_execution_uses_resolved_parameter_and_callee_identities() {
        let program = checked(
            "func add(left: i32, right: i32) -> i32 { left + right }\nfunc choose(value: i32) -> i32 { let mut total = add(value, 3); if value > 0 { total = total * 2 } total }\nfunc main() -> i32 { choose(5) }",
        );
        let add = program.function("add").expect("resolved add identity");
        let signature = program
            .resolved_signature(&add.node_id)
            .expect("resolved add signature");
        let body = program
            .resolved_body(&add.node_id)
            .expect("resolved add body");
        assert_eq!(body.parameters.len(), signature.parameters.len());
        assert!(body
            .parameters
            .iter()
            .zip(&signature.parameters)
            .all(|(local, parameter)| local.0 .0 == format!("{}/local", parameter.id.0 .0)));
        let mut interpreter = ResolvedInterpreter::new(&program);
        assert_eq!(interpreter.run_main().unwrap(), Value::Int(16));
    }

    #[test]
    fn typed_recursion_executes_from_owned_resolved_bodies() {
        let program = {
            let file = {
                let tokens = crate::lexer::Lexer::new(
                    "func factorial(value: i32) -> i32 { if value <= 1 { 1 } else { value * factorial(value - 1) } }\nfunc main() -> i32 { factorial(6) }",
                )
                .tokenize()
                .expect("lex recursion fixture");
                crate::parser::Parser::new(tokens)
                    .parse_file()
                    .expect("parse recursion fixture")
            };
            crate::core::check_program(&file).expect("check recursion fixture")
        };
        let mut interpreter = ResolvedInterpreter::new(&program);
        assert_eq!(interpreter.run_main().unwrap(), Value::Int(720));
    }

    #[test]
    fn typed_executor_fails_closed_without_raw_ast_builtin_fallback() {
        let program = checked("func main() -> i32 { len([1, 2, 3]) }");
        let main = program.function("main").expect("resolved main");
        let call_node = program
            .resolved_body(&main.node_id)
            .and_then(|body| body.root.result.as_deref())
            .map(|expression| expression.node_id.0.clone())
            .expect("typed builtin call node");
        let error = ResolvedInterpreter::new(&program)
            .run_main()
            .expect_err("unsupported typed builtin must fail closed");
        assert!(error.message().contains(&call_node));
        assert!(error.message().contains("callee"));
    }

    #[test]
    fn typed_calls_observe_checker_sorted_named_arguments() {
        let program = checked(
            "func subtract(left: i32, right: i32) -> i32 { left - right }\nfunc main() -> i32 { subtract(right = 2, left = 7) }",
        );
        let mut interpreter = ResolvedInterpreter::new(&program);
        assert_eq!(interpreter.run_main().unwrap(), Value::Int(5));
    }

    #[test]
    fn typed_loops_and_places_execute_without_surface_ast_lookup() {
        let program = checked(
            "func sum_to(limit: i32) -> i32 { let mut total = 0; let mut value = 0; while value < limit { total = total + value; value = value + 1 } total }\nfunc main() -> i32 { sum_to(6) }",
        );
        let mut interpreter = ResolvedInterpreter::new(&program);
        assert_eq!(interpreter.run_main().unwrap(), Value::Int(15));
    }

    #[test]
    fn typed_scalar_core_matches_the_surface_interpreter_oracle() {
        let source = "func fibonacci(value: i32) -> i32 { if value <= 1 { value } else { fibonacci(value - 1) + fibonacci(value - 2) } }\nfunc main() -> i32 { fibonacci(8) }";
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex differential fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse differential fixture");
        let program = crate::core::check_program(&file).expect("check differential fixture");
        let typed = ResolvedInterpreter::new(&program).run_main().unwrap();
        let surface = crate::interp::Interpreter::new(&file).run().unwrap();
        assert_eq!(typed, surface);
    }
}
