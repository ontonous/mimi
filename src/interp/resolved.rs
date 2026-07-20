//! Direct execution of checker-owned typed bodies.
//!
//! This module is the replacement execution core for the surface-AST
//! interpreter. It deliberately accepts only `CheckedProgram` identities and
//! `ResolvedBody` nodes; unsupported typed constructs fail closed instead of
//! consulting the compatibility surface program.

use super::{is_truthy, ops, values_equal, InterpError, LocalSharedInner, Value};
use crate::core::ir::{
    ResolvedBinaryOp, ResolvedFStringPart, ResolvedLambda, ResolvedValueProjection,
};
use crate::core::{
    CheckedConversion, CheckedConversionKind, CheckedProgram, NodeId, ResolvedBlock, ResolvedBody,
    ResolvedCallee, ResolvedConstValue, ResolvedExpr, ResolvedExprKind, ResolvedIndex,
    ResolvedLiteral, ResolvedLocalId, ResolvedPattern, ResolvedPatternKind, ResolvedPlace,
    ResolvedProjection, ResolvedStmt, ResolvedStmtKind, ResolvedType,
};
use std::collections::{BTreeMap, HashMap};

const MAX_TYPED_CALL_DEPTH: usize = 1024;

pub(crate) struct ResolvedInterpreter<'a> {
    program: &'a CheckedProgram,
    state: ExecutionState,
}

#[derive(Default)]
struct ExecutionState {
    frames: Vec<Frame>,
    call_depth: usize,
    output: String,
}

struct Frame {
    owner: NodeId,
    values: BTreeMap<ResolvedLocalId, Value>,
    callables: BTreeMap<ResolvedLocalId, RuntimeCallable>,
    signal: Option<ControlSignal>,
}

#[derive(Clone)]
enum RuntimeCallable {
    Direct(ResolvedCallee),
    Lambda(Box<RuntimeLambda>),
}

#[derive(Clone)]
enum RuntimeArgument {
    Missing,
    Value(Value),
    Callable(RuntimeCallable),
}

#[derive(Clone)]
struct RuntimeLambda {
    body_owner: NodeId,
    lambda: ResolvedLambda,
    captured_values: BTreeMap<ResolvedLocalId, Value>,
    captured_callables: BTreeMap<ResolvedLocalId, RuntimeCallable>,
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
        execute_call(
            self.program,
            &mut self.state,
            owner,
            arguments.into_iter().map(RuntimeArgument::Value).collect(),
        )
    }

    pub(crate) fn output(&self) -> &str {
        &self.state.output
    }
}

fn execute_call(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    owner: &NodeId,
    arguments: Vec<RuntimeArgument>,
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
    let mut callables = BTreeMap::new();
    for capture in &body.captures {
        if let Some(value) = state
            .frames
            .iter()
            .rev()
            .find_map(|frame| frame.values.get(capture))
            .cloned()
        {
            values.insert(capture.clone(), value);
        } else if let Some(callable) = state
            .frames
            .iter()
            .rev()
            .find_map(|frame| frame.callables.get(capture))
            .cloned()
        {
            callables.insert(capture.clone(), callable);
        } else {
            return Err(unsupported(
                &capture.0,
                "nested callable capture is absent from every active frame",
            ));
        }
    }
    for (local, argument) in body.parameters.iter().zip(arguments) {
        match argument {
            RuntimeArgument::Missing => {}
            RuntimeArgument::Value(value) => {
                values.insert(local.clone(), value);
            }
            RuntimeArgument::Callable(callable) => {
                callables.insert(local.clone(), callable);
            }
        }
    }
    state.frames.push(Frame {
        owner: owner.clone(),
        values,
        callables,
        signal: None,
    });
    state.call_depth += 1;

    let result = (|| {
        for index in 0..body.parameters.len() {
            if current_frame(state)?
                .values
                .contains_key(&body.parameters[index])
                || current_frame(state)?
                    .callables
                    .contains_key(&body.parameters[index])
            {
                continue;
            }
            let parameter = &signature.parameters[index];
            let default = body.default_values.get(&parameter.id).ok_or_else(|| {
                InterpError::wrong_arg_count(format!(
                    "resolved callable '{}' is missing argument '{}'",
                    owner.0, parameter.name
                ))
            })?;
            let value = eval_expr(program, state, body, default)?;
            if signal_pending(state)? {
                break;
            }
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
    if signal_pending(state)? {
        return Ok(Value::Unit);
    }
    for statement in &block.statements {
        eval_stmt(program, state, body, statement)?;
        if current_frame(state)?.signal.is_some() {
            return Ok(Value::Unit);
        }
    }
    block
        .result
        .as_deref()
        .map(|result| {
            let value = eval_expr(program, state, body, result)?;
            if signal_pending(state)? {
                Ok(Value::Unit)
            } else {
                Ok(value)
            }
        })
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
            if let Some(initializer) = initializer {
                if let Some(callable) = eval_callable(state, body, initializer)? {
                    bind_callable_pattern(state, pattern, callable)?;
                    return Ok(());
                }
            }
            let value = initializer
                .as_ref()
                .map(|value| eval_expr(program, state, body, value))
                .transpose()?
                .unwrap_or(Value::Unit);
            if signal_pending(state)? {
                return Ok(());
            }
            bind_pattern(program, state, pattern, value)?;
        }
        ResolvedStmtKind::Assign {
            target,
            value,
            conversion,
        } => {
            let value = eval_expr(program, state, body, value)?;
            if signal_pending(state)? {
                return Ok(());
            }
            let value = apply_conversion(program, conversion, value)?;
            write_place(program, state, body, target, value)?;
        }
        ResolvedStmtKind::Return { value, conversion } => {
            let value = match (value, conversion) {
                (Some(value), Some(conversion)) => {
                    let value = eval_expr(program, state, body, value)?;
                    if signal_pending(state)? {
                        return Ok(());
                    }
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
            if signal_pending(state)? {
                return Ok(());
            }
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
            if signal_pending(state)? {
                return Ok(());
            }
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
            if signal_pending(state)? {
                return Ok(());
            }
            if !try_bind_pattern(program, state, pattern, initializer)? {
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
            if signal_pending(state)? {
                return Ok(());
            }
            for value in iterable_values(&statement.node_id, iterable)? {
                bind_pattern(program, state, pattern, value)?;
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
                    current_frame(state)?.callables.remove(&place.base);
                } else {
                    write_place(program, state, body, place, Value::Unit)?;
                }
            }
        }
        ResolvedStmtKind::Contract { kind, condition } => {
            let condition = eval_expr(program, state, body, condition)?;
            if signal_pending(state)? {
                return Ok(());
            }
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
                if signal_pending(state)? {
                    return Ok(());
                }
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
                        let Some(value) = eval_child(program, state, body, value)? else {
                            return Ok(Value::Unit);
                        };
                        output.push_str(&value.to_string());
                    }
                }
            }
            Ok(Value::String(output))
        }
        ResolvedExprKind::Load(place) => read_place(program, state, body, place),
        ResolvedExprKind::Constant(identity) => constant_value(program, identity),
        ResolvedExprKind::Binary { op, left, right } => {
            let Some(left) = eval_child(program, state, body, left)? else {
                return Ok(Value::Unit);
            };
            if *op == ResolvedBinaryOp::LogicalAnd && !is_truthy(&left) {
                return Ok(Value::Bool(false));
            }
            if *op == ResolvedBinaryOp::LogicalOr && is_truthy(&left) {
                return Ok(Value::Bool(true));
            }
            let Some(right) = eval_child(program, state, body, right)? else {
                return Ok(Value::Unit);
            };
            ops::apply_binary(*op, left, right)
        }
        ResolvedExprKind::Unary { op, operand } => {
            let Some(operand) = eval_child(program, state, body, operand)? else {
                return Ok(Value::Unit);
            };
            ops::apply_unary(*op, operand)
        }
        ResolvedExprKind::Call(call) => {
            let mut arguments = Vec::with_capacity(call.arguments.len());
            for argument in &call.arguments {
                if let ResolvedExprKind::DefaultArgument {
                    callable,
                    parameter,
                } = &argument.value.kind
                {
                    if parameter != &argument.parameter
                        || !matches!(&call.callee, ResolvedCallee::Function(owner) if owner == callable)
                    {
                        return Err(unsupported(
                            &argument.value.node_id,
                            "default argument identity disagrees with its callable slot",
                        ));
                    }
                    arguments.push(RuntimeArgument::Missing);
                } else if let Some(callable) = eval_callable(state, body, &argument.value)? {
                    arguments.push(RuntimeArgument::Callable(callable));
                } else {
                    let Some(value) = eval_child(program, state, body, &argument.value)? else {
                        return Ok(Value::Unit);
                    };
                    arguments.push(RuntimeArgument::Value(apply_conversion(
                        program,
                        &argument.conversion,
                        value,
                    )?));
                }
            }
            match &call.callee {
                ResolvedCallee::Function(owner) => execute_call(program, state, owner, arguments),
                ResolvedCallee::Constructor(owner) => {
                    let arguments = require_value_arguments(&expression.node_id, arguments)?;
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
                        let name = program
                            .resolved_member_name(owner)
                            .unwrap_or(owner.0.as_str())
                            .to_string();
                        Ok(Value::Variant(name, arguments))
                    }
                }
                ResolvedCallee::Builtin(builtin) => {
                    let name = builtin.as_str();
                    let result =
                        eval_runtime_builtin(program, state, &expression.node_id, name, arguments)?;
                    if name == "push" {
                        let target = call
                            .arguments
                            .first()
                            .and_then(|argument| match &argument.value.kind {
                                ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                                    Some(place)
                                }
                                _ => None,
                            })
                            .ok_or_else(|| {
                                unsupported(
                                    &expression.node_id,
                                    "push requires a direct resolved local place",
                                )
                            })?;
                        write_place(program, state, body, target, result)?;
                        Ok(Value::Unit)
                    } else {
                        Ok(result)
                    }
                }
                ResolvedCallee::LocalClosure(local) => {
                    let callable = state
                        .frames
                        .iter()
                        .rev()
                        .find_map(|frame| frame.callables.get(local))
                        .cloned()
                        .ok_or_else(|| unsupported(&local.0, "local callable is not bound"))?;
                    execute_runtime_callable(
                        program,
                        state,
                        &expression.node_id,
                        callable,
                        arguments,
                    )
                }
                ResolvedCallee::ActorMethod { method, .. }
                | ResolvedCallee::ProtocolMethod { method, .. } => execute_call(
                    program,
                    state,
                    &NodeId(method.as_str().to_string()),
                    arguments,
                ),
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
        ResolvedExprKind::Map(entries) => {
            let mut fields = HashMap::with_capacity(entries.len());
            for (key, value) in entries {
                let Some(key) = eval_child(program, state, body, key)? else {
                    return Ok(Value::Unit);
                };
                let Value::String(key) = key else {
                    return Err(unsupported(
                        &expression.node_id,
                        "typed map keys are not strings",
                    ));
                };
                let Some(value) = eval_child(program, state, body, value)? else {
                    return Ok(Value::Unit);
                };
                fields.insert(key, value);
            }
            Ok(Value::Record(None, fields))
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
            let Some(condition) = eval_child(program, state, body, condition)? else {
                return Ok(Value::Unit);
            };
            if is_truthy(&condition) {
                eval_block(program, state, body, then_block)
            } else {
                eval_block(program, state, body, else_block)
            }
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            let Some(value) = eval_child(program, state, body, scrutinee)? else {
                return Ok(Value::Unit);
            };
            for arm in arms {
                let previous = current_frame(state)?.values.clone();
                if !try_bind_pattern(program, state, &arm.pattern, value.clone())? {
                    continue;
                }
                if let Some(guard) = &arm.guard {
                    let Some(guard) = eval_child(program, state, body, guard)? else {
                        return Ok(Value::Unit);
                    };
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
            let Some(start) = eval_child(program, state, body, start)? else {
                return Ok(Value::Unit);
            };
            let Some(end) = eval_child(program, state, body, end)? else {
                return Ok(Value::Unit);
            };
            match (start, end) {
                (Value::Int(start), Value::Int(end)) => Ok(Value::Range { start, end }),
                _ => Err(unsupported(
                    &expression.node_id,
                    "range bounds are not integers",
                )),
            }
        }
        ResolvedExprKind::Cast { value, conversion } => {
            let Some(value) = eval_child(program, state, body, value)? else {
                return Ok(Value::Unit);
            };
            apply_conversion(program, conversion, value)
        }
        ResolvedExprKind::Project { value, projection } => {
            let Some(value) = eval_child(program, state, body, value)? else {
                return Ok(Value::Unit);
            };
            read_projection(program, state, body, value, projection)
        }
        ResolvedExprKind::Old(value) => eval_expr(program, state, body, value),
        ResolvedExprKind::Record { nominal, fields } => {
            let definition = program
                .type_defs()
                .get(&NodeId(nominal.as_str().to_string()));
            let mut values = HashMap::with_capacity(fields.len());
            for field in fields {
                let name = program.resolved_member_name(&field.field).ok_or_else(|| {
                    unsupported(&field.field, "record field has no resolved display name")
                })?;
                let Some(value) = eval_child(program, state, body, &field.value)? else {
                    return Ok(Value::Unit);
                };
                let value = apply_conversion(program, &field.conversion, value)?;
                values.insert(name.to_string(), value);
            }
            Ok(Value::Record(
                Some(
                    definition
                        .map(|definition| definition.qualified_name.clone())
                        .unwrap_or_else(|| nominal.as_str().to_string()),
                ),
                values,
            ))
        }
        ResolvedExprKind::Comprehension {
            pattern,
            value,
            iterable,
            guard,
        } => {
            let Some(iterable) = eval_child(program, state, body, iterable)? else {
                return Ok(Value::Unit);
            };
            let mut values = Vec::new();
            for item in iterable_values(&expression.node_id, iterable)? {
                let previous = current_frame(state)?.values.clone();
                if try_bind_pattern(program, state, pattern, item)? {
                    let selected = match guard {
                        Some(guard) => {
                            let Some(guard) = eval_child(program, state, body, guard)? else {
                                return Ok(Value::Unit);
                            };
                            is_truthy(&guard)
                        }
                        None => true,
                    };
                    if selected {
                        let Some(value) = eval_child(program, state, body, value)? else {
                            return Ok(Value::Unit);
                        };
                        values.push(value);
                    }
                }
                current_frame(state)?.values = previous;
            }
            Ok(Value::List(values))
        }
        ResolvedExprKind::OptionalChain {
            receiver, field, ..
        } => {
            let Some(receiver) = eval_child(program, state, body, receiver)? else {
                return Ok(Value::Unit);
            };
            match receiver {
                Value::Variant(name, mut values)
                    if matches!(name.as_str(), "Some" | "Ok") && values.len() == 1 =>
                {
                    let value = values.pop().expect("checked singleton option payload");
                    let field = program.resolved_member_name(field).ok_or_else(|| {
                        unsupported(field, "optional field has no resolved display name")
                    })?;
                    let projected =
                        project_value(value, &RuntimeProjection::Field(field.to_string()))?;
                    Ok(Value::Variant("Some".into(), vec![projected]))
                }
                Value::Variant(name, _) if matches!(name.as_str(), "None" | "Err") => {
                    Ok(Value::Variant("None".into(), Vec::new()))
                }
                _ => Err(unsupported(
                    &expression.node_id,
                    "optional chain receiver is not Option/Result",
                )),
            }
        }
        ResolvedExprKind::Slice { target, start, end } => {
            let Some(target) = eval_child(program, state, body, target)? else {
                return Ok(Value::Unit);
            };
            let start = eval_optional_index(program, state, body, start.as_deref())?;
            if signal_pending(state)? {
                return Ok(Value::Unit);
            }
            let end = eval_optional_index(program, state, body, end.as_deref())?;
            if signal_pending(state)? {
                return Ok(Value::Unit);
            }
            slice_value(&expression.node_id, target, start, end)
        }
        ResolvedExprKind::Try {
            value,
            propagation_target,
        } => {
            if current_frame(state)?.owner != *propagation_target {
                return Err(unsupported(
                    &expression.node_id,
                    "Try propagation target disagrees with the active callable",
                ));
            }
            let value = eval_expr(program, state, body, value)?;
            if signal_pending(state)? {
                return Ok(Value::Unit);
            }
            match value {
                Value::Variant(name, mut payload) if matches!(name.as_str(), "Some" | "Ok") => {
                    Ok(payload.drain(..).next().unwrap_or(Value::Unit))
                }
                Value::Variant(name, payload) if matches!(name.as_str(), "None" | "Err") => {
                    current_frame(state)?.signal =
                        Some(ControlSignal::Return(Value::Variant(name, payload)));
                    Ok(Value::Unit)
                }
                value @ Value::Error(_) => {
                    current_frame(state)?.signal = Some(ControlSignal::Return(value));
                    Ok(Value::Unit)
                }
                _ => Err(InterpError::type_mismatch(
                    "Try operator requires an Option or Result value",
                )),
            }
        }
        ResolvedExprKind::TypeOf(_)
        | ResolvedExprKind::Spawn(_)
        | ResolvedExprKind::Await(_)
        | ResolvedExprKind::Lambda(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
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
    let mut evaluated = Vec::with_capacity(values.len());
    for value in values {
        let Some(value) = eval_child(program, state, body, value)? else {
            return Ok(Vec::new());
        };
        evaluated.push(value);
    }
    Ok(evaluated)
}

fn eval_callable(
    state: &mut ExecutionState,
    body: &ResolvedBody,
    expression: &ResolvedExpr,
) -> Result<Option<RuntimeCallable>, InterpError> {
    match &expression.kind {
        ResolvedExprKind::Callable(callee) => Ok(Some(RuntimeCallable::Direct(callee.clone()))),
        ResolvedExprKind::Load(place) if place.projections.is_empty() => Ok(state
            .frames
            .iter()
            .rev()
            .find_map(|frame| frame.callables.get(&place.base))
            .cloned()),
        ResolvedExprKind::Lambda(lambda) => {
            let mut captured_values = BTreeMap::new();
            let mut captured_callables = BTreeMap::new();
            for capture in &lambda.captures {
                if let Some(value) = state
                    .frames
                    .iter()
                    .rev()
                    .find_map(|frame| frame.values.get(capture))
                    .cloned()
                {
                    captured_values.insert(capture.clone(), value);
                } else if let Some(callable) = state
                    .frames
                    .iter()
                    .rev()
                    .find_map(|frame| frame.callables.get(capture))
                    .cloned()
                {
                    captured_callables.insert(capture.clone(), callable);
                } else {
                    return Err(unsupported(
                        &capture.0,
                        "lambda capture is absent from every active frame",
                    ));
                }
            }
            Ok(Some(RuntimeCallable::Lambda(Box::new(RuntimeLambda {
                body_owner: body.owner.clone(),
                lambda: lambda.as_ref().clone(),
                captured_values,
                captured_callables,
            }))))
        }
        _ => Ok(None),
    }
}

fn bind_callable_pattern(
    state: &mut ExecutionState,
    pattern: &ResolvedPattern,
    callable: RuntimeCallable,
) -> Result<(), InterpError> {
    match &pattern.kind {
        ResolvedPatternKind::Binding { local, .. } => {
            let frame = current_frame(state)?;
            frame.values.remove(local);
            frame.callables.insert(local.clone(), callable);
            Ok(())
        }
        ResolvedPatternKind::Wildcard => Ok(()),
        _ => Err(unsupported(
            &pattern.node_id,
            "callable binding requires one resolved local",
        )),
    }
}

fn execute_runtime_callable(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    call_node: &NodeId,
    callable: RuntimeCallable,
    arguments: Vec<RuntimeArgument>,
) -> Result<Value, InterpError> {
    match callable {
        RuntimeCallable::Direct(ResolvedCallee::Function(owner)) => {
            execute_call(program, state, &owner, arguments)
        }
        RuntimeCallable::Direct(ResolvedCallee::Builtin(builtin)) => {
            eval_runtime_builtin(program, state, call_node, builtin.as_str(), arguments)
        }
        RuntimeCallable::Lambda(runtime) => {
            let RuntimeLambda {
                body_owner,
                lambda,
                mut captured_values,
                mut captured_callables,
            } = *runtime;
            if state.call_depth >= MAX_TYPED_CALL_DEPTH {
                return Err(InterpError::new(
                    "typed interpreter recursion limit exceeded",
                ));
            }
            if arguments.len() != lambda.parameters.len() {
                return Err(InterpError::wrong_arg_count(format!(
                    "resolved lambda '{}' expects {} arguments, got {}",
                    lambda.owner.0,
                    lambda.parameters.len(),
                    arguments.len()
                )));
            }
            let owner_body = program
                .resolved_body(&body_owner)
                .ok_or_else(|| unsupported(&body_owner, "lambda owner body is absent"))?;
            for (parameter, argument) in lambda.parameters.iter().zip(arguments) {
                match argument {
                    RuntimeArgument::Missing => {
                        return Err(unsupported(
                            call_node,
                            "lambda call contains a default argument",
                        ));
                    }
                    RuntimeArgument::Value(value) => {
                        captured_values.insert(parameter.clone(), value);
                    }
                    RuntimeArgument::Callable(callable) => {
                        captured_callables.insert(parameter.clone(), callable);
                    }
                }
            }
            state.frames.push(Frame {
                owner: body_owner,
                values: captured_values,
                callables: captured_callables,
                signal: None,
            });
            state.call_depth += 1;
            let result = (|| {
                let value = eval_block(program, state, owner_body, &lambda.body)?;
                match current_frame(state)?.signal.take() {
                    Some(ControlSignal::Return(value)) => Ok(value),
                    Some(ControlSignal::Break(_)) | Some(ControlSignal::Continue) => Err(
                        unsupported(&lambda.owner, "loop control escaped the lambda root"),
                    ),
                    None => Ok(value),
                }
            })();
            state.call_depth -= 1;
            state.frames.pop();
            result.map_err(|error| error.in_func(lambda.owner.0))
        }
        RuntimeCallable::Direct(_) => Err(unsupported(
            call_node,
            "first-class callable kind is outside the typed execution subset",
        )),
    }
}

fn require_value_arguments(
    node: &NodeId,
    arguments: Vec<RuntimeArgument>,
) -> Result<Vec<Value>, InterpError> {
    arguments
        .into_iter()
        .enumerate()
        .map(|(index, argument)| match argument {
            RuntimeArgument::Value(value) => Ok(value),
            RuntimeArgument::Missing => Err(unsupported(
                node,
                &format!("non-function callee has a default argument at slot {index}"),
            )),
            RuntimeArgument::Callable(_) => Err(unsupported(
                node,
                &format!("callee does not accept a callable at slot {index}"),
            )),
        })
        .collect()
}

fn constant_value(program: &CheckedProgram, identity: &NodeId) -> Result<Value, InterpError> {
    if identity.0 == "builtin:value:None" {
        return Ok(Value::Variant("None".into(), Vec::new()));
    }
    if let Some(constant) = program.constants().get(identity) {
        return match &constant.value {
            ResolvedConstValue::Int(value) => Ok(Value::Int(*value)),
            ResolvedConstValue::Float(value) => Ok(Value::Float(*value)),
            ResolvedConstValue::Bool(value) => Ok(Value::Bool(*value)),
            ResolvedConstValue::String(value) => Ok(Value::String(value.clone())),
            ResolvedConstValue::Unit => Ok(Value::Unit),
            ResolvedConstValue::Complex => Err(unsupported(
                identity,
                "constant initializer was not materialized by the checker",
            )),
        };
    }
    if let Some(name) = program.resolved_member_name(identity) {
        return Ok(Value::Variant(name.to_string(), Vec::new()));
    }
    Err(unsupported(
        identity,
        "constant identity is absent from the checked constant catalog",
    ))
}

fn eval_optional_index(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    expression: Option<&ResolvedExpr>,
) -> Result<Option<usize>, InterpError> {
    let Some(expression) = expression else {
        return Ok(None);
    };
    let Some(value) = eval_child(program, state, body, expression)? else {
        return Ok(None);
    };
    match value {
        Value::Int(index) => usize::try_from(index)
            .map(Some)
            .map_err(|_| InterpError::index_out_of_bounds("negative slice bound")),
        _ => Err(unsupported(
            &expression.node_id,
            "slice bound is not an integer",
        )),
    }
}

fn slice_value(
    node: &NodeId,
    value: Value,
    start: Option<usize>,
    end: Option<usize>,
) -> Result<Value, InterpError> {
    match value {
        Value::List(source) | Value::Array(source) => {
            let start = start.unwrap_or(0);
            let end = end.unwrap_or(source.len());
            if start > end || end > source.len() {
                return Err(InterpError::index_out_of_bounds(format!(
                    "slice {start}..{end} exceeds length {}",
                    source.len()
                )));
            }
            Ok(Value::Slice { source, start, end })
        }
        Value::Slice {
            source,
            start: parent_start,
            end: parent_end,
        } => {
            let length = parent_end - parent_start;
            let start = start.unwrap_or(0);
            let end = end.unwrap_or(length);
            if start > end || end > length {
                return Err(InterpError::index_out_of_bounds(format!(
                    "slice {start}..{end} exceeds length {length}"
                )));
            }
            Ok(Value::Slice {
                source,
                start: parent_start + start,
                end: parent_start + end,
            })
        }
        Value::String(value) => {
            let characters = value.chars().collect::<Vec<_>>();
            let start = start.unwrap_or(0);
            let end = end.unwrap_or(characters.len());
            if start > end || end > characters.len() {
                return Err(InterpError::index_out_of_bounds(format!(
                    "slice {start}..{end} exceeds length {}",
                    characters.len()
                )));
            }
            Ok(Value::String(characters[start..end].iter().collect()))
        }
        _ => Err(unsupported(node, "slice target is not a sequence")),
    }
}

fn eval_runtime_builtin(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    node: &NodeId,
    name: &str,
    arguments: Vec<RuntimeArgument>,
) -> Result<Value, InterpError> {
    match name {
        "map" | "filter" | "reduce" => {
            eval_collection_callable(program, state, node, name, arguments)
        }
        "builtin.method.option.map"
        | "builtin.method.option.and_then"
        | "builtin.method.result.map"
        | "builtin.method.result.and_then"
        | "builtin.method.result.map_err" => {
            eval_variant_callable(program, state, node, name, arguments)
        }
        _ => eval_builtin(state, node, name, require_value_arguments(node, arguments)?),
    }
}

fn eval_collection_callable(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    node: &NodeId,
    name: &str,
    arguments: Vec<RuntimeArgument>,
) -> Result<Value, InterpError> {
    let expected = if name == "reduce" { 3 } else { 2 };
    if arguments.len() != expected {
        return Err(InterpError::wrong_arg_count(format!(
            "builtin '{name}' expects {expected} arguments, got {}",
            arguments.len()
        )));
    }
    let values = match &arguments[0] {
        RuntimeArgument::Value(Value::List(values)) => values.clone(),
        _ => return Err(builtin_type_error(name, "a list and callable")),
    };
    let callable = match &arguments[1] {
        RuntimeArgument::Callable(callable) => callable.clone(),
        _ => return Err(builtin_type_error(name, "a list and callable")),
    };
    match name {
        "map" => values
            .into_iter()
            .map(|value| {
                execute_runtime_callable(
                    program,
                    state,
                    node,
                    callable.clone(),
                    vec![RuntimeArgument::Value(value)],
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        "filter" => {
            let mut selected = Vec::new();
            for value in values {
                let predicate = execute_runtime_callable(
                    program,
                    state,
                    node,
                    callable.clone(),
                    vec![RuntimeArgument::Value(value.clone())],
                )?;
                if is_truthy(&predicate) {
                    selected.push(value);
                }
            }
            Ok(Value::List(selected))
        }
        "reduce" => {
            let mut accumulator = match &arguments[2] {
                RuntimeArgument::Value(value) => value.clone(),
                _ => return Err(builtin_type_error(name, "a concrete initial value")),
            };
            for value in values {
                accumulator = execute_runtime_callable(
                    program,
                    state,
                    node,
                    callable.clone(),
                    vec![
                        RuntimeArgument::Value(accumulator),
                        RuntimeArgument::Value(value),
                    ],
                )?;
            }
            Ok(accumulator)
        }
        _ => Err(unsupported(node, "unknown canonical collection callable")),
    }
}

fn eval_variant_callable(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    node: &NodeId,
    name: &str,
    arguments: Vec<RuntimeArgument>,
) -> Result<Value, InterpError> {
    if arguments.len() != 2 {
        return Err(InterpError::wrong_arg_count(format!(
            "builtin '{name}' expects a receiver and callable"
        )));
    }
    let (variant, payload) = match &arguments[0] {
        RuntimeArgument::Value(Value::Variant(variant, payload)) => {
            (variant.clone(), payload.clone())
        }
        _ => return Err(builtin_type_error(name, "an Option/Result receiver")),
    };
    let callable = match &arguments[1] {
        RuntimeArgument::Callable(callable) => callable.clone(),
        _ => return Err(builtin_type_error(name, "a callable argument")),
    };
    let method = name
        .rsplit_once('.')
        .map(|(_, method)| method)
        .unwrap_or(name);
    let should_call = match method {
        "map" | "and_then" => matches!(variant.as_str(), "Some" | "Ok"),
        "map_err" => variant == "Err",
        _ => false,
    };
    if !should_call {
        return Ok(Value::Variant(variant, payload));
    }
    let argument = payload.first().cloned().unwrap_or(Value::Unit);
    let mapped = execute_runtime_callable(
        program,
        state,
        node,
        callable,
        vec![RuntimeArgument::Value(argument)],
    )?;
    if method == "and_then" {
        Ok(mapped)
    } else {
        Ok(Value::Variant(variant, vec![mapped]))
    }
}

fn eval_builtin(
    state: &mut ExecutionState,
    node: &NodeId,
    name: &str,
    arguments: Vec<Value>,
) -> Result<Value, InterpError> {
    if let Some(method) = name.strip_prefix("builtin.method.") {
        return eval_builtin_method(node, method, arguments);
    }
    match name {
        "Some" | "Ok" | "Err" => {
            expect_arity(name, &arguments, 1)?;
            Ok(Value::Variant(name.into(), arguments))
        }
        "None" => {
            expect_arity(name, &arguments, 0)?;
            Ok(Value::Variant("None".into(), Vec::new()))
        }
        "print" | "println" => {
            let rendered = arguments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            state.output.push_str(&rendered);
            if name == "println" {
                state.output.push('\n');
            }
            Ok(Value::Unit)
        }
        "assert" => {
            if arguments.is_empty() || arguments.len() > 2 {
                return Err(InterpError::builtin_error(
                    "assert expects a condition and optional message",
                ));
            }
            if !is_truthy(&arguments[0]) {
                let message = arguments
                    .get(1)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| arguments[0].to_string());
                return Err(InterpError::builtin_error(format!(
                    "assertion failed: {message}"
                )));
            }
            Ok(Value::Unit)
        }
        "assert_eq" | "assert_ne" => {
            expect_arity(name, &arguments, 2)?;
            let equal = values_equal(&arguments[0], &arguments[1]);
            if (name == "assert_eq" && !equal) || (name == "assert_ne" && equal) {
                return Err(InterpError::builtin_error(format!(
                    "{name} failed: {} and {}",
                    arguments[0], arguments[1]
                )));
            }
            Ok(Value::Unit)
        }
        "range" => match arguments.as_slice() {
            [Value::Int(start), Value::Int(end)] => {
                Ok(Value::List((*start..*end).map(Value::Int).collect()))
            }
            _ => Err(builtin_type_error(name, "two integers")),
        },
        "len" => {
            expect_arity(name, &arguments, 1)?;
            sequence_len(&arguments[0]).map(|length| Value::Int(length as i64))
        }
        "to_string" | "int_to_string" | "float_to_string" => {
            expect_arity(name, &arguments, 1)?;
            Ok(Value::String(arguments[0].to_string()))
        }
        "abs" => {
            expect_arity(name, &arguments, 1)?;
            match arguments[0] {
                Value::Int(value) => value
                    .checked_abs()
                    .map(Value::Int)
                    .ok_or_else(|| InterpError::integer_overflow("absolute value overflow")),
                Value::Float(value) => Ok(Value::Float(value.abs())),
                _ => Err(builtin_type_error(name, "one number")),
            }
        }
        "sqrt" => {
            expect_arity(name, &arguments, 1)?;
            let value = match arguments[0] {
                Value::Int(value) => (value as f64).sqrt(),
                Value::Float(value) => value.sqrt(),
                _ => return Err(builtin_type_error(name, "one number")),
            };
            if value.is_finite() {
                Ok(Value::Float(value))
            } else {
                Err(InterpError::float_error("sqrt produced a non-finite value"))
            }
        }
        "min" | "max" => numeric_min_max(name, &arguments),
        "contains" => contains_value(name, &arguments),
        "format" => format_values(&arguments),
        "option_value_or" => option_value_or(name, &arguments),
        "str_trim" => string_unary(name, &arguments, |value| value.trim().to_string()),
        "str_to_upper" => string_unary(name, &arguments, str::to_uppercase),
        "str_to_lower" => string_unary(name, &arguments, str::to_lowercase),
        "str_parse_int" | "string_to_int" => {
            expect_arity(name, &arguments, 1)?;
            match &arguments[0] {
                Value::String(value) => Ok(value
                    .trim()
                    .parse::<i64>()
                    .map(|value| Value::Tuple(vec![Value::Bool(true), Value::Int(value)]))
                    .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Int(0)]))),
                _ => Err(builtin_type_error(name, "one string")),
            }
        }
        "str_parse_float" => {
            expect_arity(name, &arguments, 1)?;
            match &arguments[0] {
                Value::String(value) => Ok(value
                    .trim()
                    .parse::<f64>()
                    .map(|value| Value::Tuple(vec![Value::Bool(true), Value::Float(value)]))
                    .unwrap_or_else(|_| Value::Tuple(vec![Value::Bool(false), Value::Float(0.0)]))),
                _ => Err(builtin_type_error(name, "one string")),
            }
        }
        "str_contains" | "str_starts_with" | "str_ends_with" => string_predicate(name, &arguments),
        "str_split" => match arguments.as_slice() {
            [Value::String(value), Value::String(separator)] => Ok(Value::List(
                value
                    .split(separator)
                    .map(|part| Value::String(part.to_string()))
                    .collect(),
            )),
            _ => Err(builtin_type_error(name, "two strings")),
        },
        "str_join" => match arguments.as_slice() {
            [Value::List(values), Value::String(separator)] => values
                .iter()
                .map(|value| match value {
                    Value::String(value) => Ok(value.as_str()),
                    _ => Err(builtin_type_error(name, "a string list and separator")),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|values| Value::String(values.join(separator))),
            _ => Err(builtin_type_error(name, "a string list and separator")),
        },
        "str_char_at" => match arguments.as_slice() {
            [Value::String(value), Value::Int(index)] => value
                .chars()
                .nth(*index as usize)
                .map(|character| Value::String(character.to_string()))
                .ok_or_else(|| {
                    InterpError::index_out_of_bounds(format!(
                        "str_char_at index {index} exceeds character length {}",
                        value.chars().count()
                    ))
                }),
            _ => Err(builtin_type_error(name, "a string and integer index")),
        },
        "str_substring" => match arguments.as_slice() {
            [Value::String(value), Value::Int(start), Value::Int(end)] => {
                let characters = value.chars().collect::<Vec<_>>();
                let start = (*start as usize).min(characters.len());
                let end = (*end as usize).min(characters.len());
                if start > end {
                    Err(InterpError::index_out_of_bounds(
                        "str_substring start exceeds end",
                    ))
                } else {
                    Ok(Value::String(characters[start..end].iter().collect()))
                }
            }
            _ => Err(builtin_type_error(name, "a string and two integer indices")),
        },
        "str_replace" => match arguments.as_slice() {
            [Value::String(value), Value::String(from), Value::String(to)] => {
                Ok(Value::String(value.replace(from, to)))
            }
            _ => Err(builtin_type_error(name, "three strings")),
        },
        "str_repeat" => match arguments.as_slice() {
            [Value::String(value), Value::Int(count)] if *count >= 0 => {
                Ok(Value::String(value.repeat(*count as usize)))
            }
            _ => Err(builtin_type_error(
                name,
                "a string and non-negative integer",
            )),
        },
        "str_index_of" => match arguments.as_slice() {
            [Value::String(value), Value::String(needle)] => Ok(value
                .find(needle)
                .map(|byte_index| {
                    Value::Variant(
                        "Some".into(),
                        vec![Value::Int(value[..byte_index].chars().count() as i64)],
                    )
                })
                .unwrap_or_else(|| Value::Variant("None".into(), Vec::new()))),
            _ => Err(builtin_type_error(name, "two strings")),
        },
        "char_code" => match arguments.as_slice() {
            [Value::String(value), Value::Int(index)] => value
                .chars()
                .nth(*index as usize)
                .map(|character| Value::Int(character as i64))
                .ok_or_else(|| {
                    InterpError::index_out_of_bounds(format!(
                        "char_code index {index} exceeds character length {}",
                        value.chars().count()
                    ))
                }),
            _ => Err(builtin_type_error(name, "a string and integer index")),
        },
        "chr" => match arguments.as_slice() {
            [Value::Int(code)] => u32::try_from(*code)
                .ok()
                .and_then(char::from_u32)
                .map(|character| Value::String(character.to_string()))
                .ok_or_else(|| InterpError::builtin_error(format!("invalid code point {code}"))),
            _ => Err(builtin_type_error(name, "one integer code point")),
        },
        "push" => match arguments.as_slice() {
            [Value::List(values), value] => {
                let mut result = values.clone();
                result.push(value.clone());
                Ok(Value::List(result))
            }
            _ => Err(builtin_type_error(name, "a list and compatible value")),
        },
        "pop" => match arguments.as_slice() {
            [Value::List(values)] => values
                .last()
                .cloned()
                .ok_or_else(|| InterpError::builtin_error("pop from empty list")),
            _ => Err(builtin_type_error(name, "one list")),
        },
        "sort" | "sort_f64" | "sort_str" => sort_values(name, &arguments),
        "reverse" => match arguments.as_slice() {
            [Value::List(values)] => {
                let mut result = values.clone();
                result.reverse();
                Ok(Value::List(result))
            }
            _ => Err(builtin_type_error(name, "one list")),
        },
        "flatten" => match arguments.as_slice() {
            [Value::List(values)] => Ok(Value::List(
                values
                    .iter()
                    .flat_map(|value| match value {
                        Value::List(inner) => inner.clone(),
                        value => vec![value.clone()],
                    })
                    .collect(),
            )),
            _ => Err(builtin_type_error(name, "one list")),
        },
        "zip" => match arguments.as_slice() {
            [Value::List(left), Value::List(right)] => Ok(Value::List(
                left.iter()
                    .zip(right)
                    .map(|(left, right)| Value::Tuple(vec![left.clone(), right.clone()]))
                    .collect(),
            )),
            _ => Err(builtin_type_error(name, "two lists")),
        },
        "enumerate" => match arguments.as_slice() {
            [Value::List(values)] => Ok(Value::List(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        Value::Tuple(vec![Value::Int(index as i64), value.clone()])
                    })
                    .collect(),
            )),
            _ => Err(builtin_type_error(name, "one list")),
        },
        "sum" => sum_values(name, &arguments),
        "keys" | "values" | "has_key" | "map_new" | "map_get" | "map_set" | "map_remove"
        | "map_size" | "map_from_list" => map_value_builtin(name, &arguments),
        _ => Err(unsupported(
            node,
            &format!("builtin '{name}' is outside the typed execution subset"),
        )),
    }
}

fn eval_builtin_method(
    node: &NodeId,
    identity: &str,
    arguments: Vec<Value>,
) -> Result<Value, InterpError> {
    let (family, method) = identity
        .split_once('.')
        .ok_or_else(|| unsupported(node, "builtin method identity has no family separator"))?;
    match family {
        "option" | "result" => option_result_method(node, method, arguments),
        "string" => string_method(node, method, arguments),
        "list" if method == "len" => {
            expect_arity(identity, &arguments, 1)?;
            sequence_len(&arguments[0]).map(|length| Value::Int(length as i64))
        }
        "set" => set_method(node, method, arguments),
        "shared" | "local_shared" => strong_ownership_method(node, method, arguments),
        "weak" | "weak_local" => weak_ownership_method(node, method, arguments),
        _ => Err(unsupported(
            node,
            &format!("builtin method '{identity}' is outside the typed execution subset"),
        )),
    }
}

fn option_result_method(
    node: &NodeId,
    method: &str,
    arguments: Vec<Value>,
) -> Result<Value, InterpError> {
    let Some(Value::Variant(variant, payload)) = arguments.first() else {
        return Err(builtin_type_error(method, "an Option/Result receiver"));
    };
    match method {
        "unwrap" | "expect" => {
            let expected = if method == "expect" { 2 } else { 1 };
            expect_arity(method, &arguments, expected)?;
            match variant.as_str() {
                "Some" | "Ok" => payload
                    .first()
                    .cloned()
                    .ok_or_else(|| builtin_type_error(method, "a payload")),
                "None" | "Err" => Err(InterpError::builtin_error(
                    arguments
                        .get(1)
                        .map(ToString::to_string)
                        .unwrap_or_else(|| format!("called {method} on {variant}")),
                )),
                _ => Err(builtin_type_error(method, "an Option/Result receiver")),
            }
        }
        "unwrap_or" => {
            expect_arity(method, &arguments, 2)?;
            if matches!(variant.as_str(), "Some" | "Ok") {
                payload
                    .first()
                    .cloned()
                    .ok_or_else(|| builtin_type_error(method, "a payload"))
            } else {
                Ok(arguments[1].clone())
            }
        }
        "is_some" | "is_ok" => {
            expect_arity(method, &arguments, 1)?;
            Ok(Value::Bool(matches!(variant.as_str(), "Some" | "Ok")))
        }
        "is_none" | "is_err" => {
            expect_arity(method, &arguments, 1)?;
            Ok(Value::Bool(matches!(variant.as_str(), "None" | "Err")))
        }
        "ok_or" => {
            expect_arity(method, &arguments, 2)?;
            match variant.as_str() {
                "Some" => Ok(Value::Variant("Ok".into(), payload.clone())),
                "None" => Ok(Value::Variant("Err".into(), vec![arguments[1].clone()])),
                _ => Err(builtin_type_error(method, "an Option receiver")),
            }
        }
        "deref" => {
            expect_arity(method, &arguments, 1)?;
            let value = match variant.as_str() {
                "Some" | "Ok" => payload
                    .first()
                    .cloned()
                    .ok_or_else(|| builtin_type_error(method, "a payload"))?,
                "None" | "Err" => {
                    return Err(InterpError::builtin_error(format!(
                        "called deref on {variant}"
                    )))
                }
                _ => return Err(builtin_type_error(method, "an Option receiver")),
            };
            read_owned_value(method, value)
        }
        _ => Err(unsupported(
            node,
            &format!("Option/Result method '{method}' requires typed callable support"),
        )),
    }
}

fn strong_ownership_method(
    node: &NodeId,
    method: &str,
    arguments: Vec<Value>,
) -> Result<Value, InterpError> {
    expect_arity(method, &arguments, 1)?;
    match method {
        "clone" => Ok(arguments[0].clone()),
        "deref" | "inner" => read_owned_value(method, arguments[0].clone()),
        _ => Err(unsupported(
            node,
            &format!("strong ownership method '{method}' is unknown"),
        )),
    }
}

fn weak_ownership_method(
    node: &NodeId,
    method: &str,
    arguments: Vec<Value>,
) -> Result<Value, InterpError> {
    expect_arity(method, &arguments, 1)?;
    if method != "upgrade" {
        return Err(unsupported(
            node,
            &format!("weak ownership method '{method}' is unknown"),
        ));
    }
    let upgraded = match &arguments[0] {
        Value::WeakShared(value) => value.upgrade().map(Value::Shared),
        Value::WeakLocal(value) => value.upgrade().map(Value::LocalShared),
        _ => return Err(builtin_type_error(method, "a weak ownership receiver")),
    };
    Ok(match upgraded {
        Some(value) => Value::Variant("Some".into(), vec![value]),
        None => Value::Variant("None".into(), Vec::new()),
    })
}

fn read_owned_value(name: &str, value: Value) -> Result<Value, InterpError> {
    match value {
        Value::Shared(value) | Value::Ref(value) | Value::RefMut(value) => value
            .read()
            .map_err(|_| InterpError::lock_error(format!("poisoned {name} value")))
            .map(|value| value.clone()),
        Value::LocalShared(value) => value
            .lock()
            .map_err(|_| InterpError::lock_error(format!("poisoned {name} value")))
            .map(|value| value.clone()),
        _ => Err(builtin_type_error(name, "a strong ownership value")),
    }
}

fn string_method(node: &NodeId, method: &str, arguments: Vec<Value>) -> Result<Value, InterpError> {
    match method {
        "len" => {
            expect_arity(method, &arguments, 1)?;
            sequence_len(&arguments[0]).map(|length| Value::Int(length as i64))
        }
        "trim" => string_unary(method, &arguments, |value| value.trim().to_string()),
        "to_upper" => string_unary(method, &arguments, str::to_uppercase),
        "to_lower" => string_unary(method, &arguments, str::to_lowercase),
        "contains" | "starts_with" | "ends_with" => string_predicate(method, &arguments),
        "split" => eval_builtin(&mut ExecutionState::default(), node, "str_split", arguments),
        "repeat" => match arguments.as_slice() {
            [Value::String(value), Value::Int(count)] if *count >= 0 => {
                Ok(Value::String(value.repeat(*count as usize)))
            }
            _ => Err(builtin_type_error(
                method,
                "a string and non-negative integer",
            )),
        },
        _ => Err(unsupported(
            node,
            &format!("string method '{method}' is outside the typed execution subset"),
        )),
    }
}

fn set_method(node: &NodeId, method: &str, arguments: Vec<Value>) -> Result<Value, InterpError> {
    let Some(Value::Set(values)) = arguments.first() else {
        return Err(builtin_type_error(method, "a set receiver"));
    };
    match method {
        "size" | "len" => {
            expect_arity(method, &arguments, 1)?;
            Ok(Value::Int(values.len() as i64))
        }
        "is_empty" => {
            expect_arity(method, &arguments, 1)?;
            Ok(Value::Bool(values.is_empty()))
        }
        "contains" => {
            expect_arity(method, &arguments, 2)?;
            Ok(Value::Bool(
                values
                    .iter()
                    .any(|value| values_equal(value, &arguments[1])),
            ))
        }
        "insert" => {
            expect_arity(method, &arguments, 2)?;
            let mut result = values.clone();
            if !result
                .iter()
                .any(|value| values_equal(value, &arguments[1]))
            {
                result.push(arguments[1].clone());
            }
            Ok(Value::Set(result))
        }
        "remove" => {
            expect_arity(method, &arguments, 2)?;
            Ok(Value::Set(
                values
                    .iter()
                    .filter(|value| !values_equal(value, &arguments[1]))
                    .cloned()
                    .collect(),
            ))
        }
        "to_list" => {
            expect_arity(method, &arguments, 1)?;
            Ok(Value::List(values.clone()))
        }
        _ => Err(unsupported(
            node,
            &format!("set method '{method}' is outside the typed execution subset"),
        )),
    }
}

fn sequence_len(value: &Value) -> Result<usize, InterpError> {
    match value {
        Value::String(value) => Ok(value.chars().count()),
        Value::List(values) | Value::Array(values) | Value::Tuple(values) | Value::Set(values) => {
            Ok(values.len())
        }
        Value::Slice { start, end, .. } => Ok(end - start),
        Value::Record(_, values) => Ok(values.len()),
        _ => Err(builtin_type_error("len", "a finite container")),
    }
}

fn numeric_min_max(name: &str, arguments: &[Value]) -> Result<Value, InterpError> {
    match arguments {
        [Value::Int(left), Value::Int(right)] if name == "min" => Ok(Value::Int(*left.min(right))),
        [Value::Int(left), Value::Int(right)] => Ok(Value::Int(*left.max(right))),
        [Value::Float(left), Value::Float(right)] if name == "min" => {
            Ok(Value::Float(left.min(*right)))
        }
        [Value::Float(left), Value::Float(right)] => Ok(Value::Float(left.max(*right))),
        _ => Err(builtin_type_error(name, "two numbers of the same type")),
    }
}

fn sort_values(name: &str, arguments: &[Value]) -> Result<Value, InterpError> {
    let [Value::List(values)] = arguments else {
        return Err(builtin_type_error(name, "one list"));
    };
    let mut result = values.clone();
    result.sort_by(|left, right| match (left, right) {
        (Value::Int(left), Value::Int(right)) => left.cmp(right),
        (Value::Float(left), Value::Float(right)) => {
            left.partial_cmp(right)
                .unwrap_or_else(|| match (left.is_nan(), right.is_nan()) {
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    _ => std::cmp::Ordering::Equal,
                })
        }
        (Value::String(left), Value::String(right)) => left.cmp(right),
        _ => std::cmp::Ordering::Equal,
    });
    Ok(Value::List(result))
}

fn sum_values(name: &str, arguments: &[Value]) -> Result<Value, InterpError> {
    let [Value::List(values)] = arguments else {
        return Err(builtin_type_error(name, "one numeric list"));
    };
    let mut integer = 0i64;
    let mut float = 0.0;
    let mut has_float = false;
    for value in values {
        match value {
            Value::Int(value) => {
                integer = integer
                    .checked_add(*value)
                    .ok_or_else(|| InterpError::integer_overflow("sum result exceeds i64 range"))?;
            }
            Value::Float(value) => {
                float += value;
                has_float = true;
            }
            _ => return Err(builtin_type_error(name, "one numeric list")),
        }
    }
    if has_float {
        Ok(Value::Float(float + integer as f64))
    } else {
        Ok(Value::Int(integer))
    }
}

fn map_value_builtin(name: &str, arguments: &[Value]) -> Result<Value, InterpError> {
    match name {
        "map_new" => {
            expect_arity(name, arguments, 0)?;
            Ok(Value::Record(None, HashMap::new()))
        }
        "map_get" => match arguments {
            [Value::Record(_, fields), Value::String(key)] => Ok(fields
                .get(key)
                .cloned()
                .map(|value| Value::Tuple(vec![Value::Bool(true), value]))
                .unwrap_or_else(|| Value::Tuple(vec![Value::Bool(false), Value::Int(0)]))),
            _ => Err(builtin_type_error(name, "a record and string key")),
        },
        "map_set" => match arguments {
            [Value::Record(nominal, fields), Value::String(key), value] => {
                let mut result = fields.clone();
                result.insert(key.clone(), value.clone());
                Ok(Value::Record(nominal.clone(), result))
            }
            _ => Err(builtin_type_error(name, "a record, string key, and value")),
        },
        "map_remove" => match arguments {
            [Value::Record(nominal, fields), Value::String(key)] => {
                let mut result = fields.clone();
                result.remove(key);
                Ok(Value::Record(nominal.clone(), result))
            }
            _ => Err(builtin_type_error(name, "a record and string key")),
        },
        "map_size" => match arguments {
            [Value::Record(_, fields)] => Ok(Value::Int(fields.len() as i64)),
            _ => Err(builtin_type_error(name, "one record")),
        },
        "map_from_list" => match arguments {
            [Value::List(pairs)] => {
                let mut result = HashMap::new();
                for pair in pairs {
                    match pair {
                        Value::Tuple(values) if values.len() == 2 => {
                            let Value::String(key) = &values[0] else {
                                return Err(builtin_type_error(
                                    name,
                                    "a list of (string, value) tuples",
                                ));
                            };
                            result.insert(key.clone(), values[1].clone());
                        }
                        _ => {
                            return Err(builtin_type_error(
                                name,
                                "a list of (string, value) tuples",
                            ));
                        }
                    }
                }
                Ok(Value::Record(None, result))
            }
            _ => Err(builtin_type_error(name, "a list of (string, value) tuples")),
        },
        "has_key" => match arguments {
            [Value::Record(_, fields), Value::String(key)] => {
                Ok(Value::Bool(fields.contains_key(key)))
            }
            _ => Err(builtin_type_error(name, "a record and string key")),
        },
        "keys" | "values" => match arguments {
            [Value::Record(_, fields)] => {
                let mut entries = fields.iter().collect::<Vec<_>>();
                entries.sort_by(|(left, _), (right, _)| left.cmp(right));
                Ok(Value::List(
                    entries
                        .into_iter()
                        .map(|(key, value)| {
                            if name == "keys" {
                                Value::String(key.clone())
                            } else {
                                value.clone()
                            }
                        })
                        .collect(),
                ))
            }
            _ => Err(builtin_type_error(name, "one record")),
        },
        _ => Err(InterpError::builtin_error(format!(
            "unknown map builtin '{name}'"
        ))),
    }
}

fn contains_value(name: &str, arguments: &[Value]) -> Result<Value, InterpError> {
    match arguments {
        [Value::String(value), Value::String(needle)] => Ok(Value::Bool(value.contains(needle))),
        [Value::List(values) | Value::Array(values) | Value::Set(values), needle] => Ok(
            Value::Bool(values.iter().any(|value| values_equal(value, needle))),
        ),
        _ => Err(builtin_type_error(name, "a container and compatible value")),
    }
}

fn format_values(arguments: &[Value]) -> Result<Value, InterpError> {
    let Some(Value::String(template)) = arguments.first() else {
        return Err(builtin_type_error("format", "a template string"));
    };
    let mut output = String::new();
    let mut rest = template.as_str();
    let mut index = 1;
    while let Some(position) = rest.find("{}") {
        output.push_str(&rest[..position]);
        if let Some(value) = arguments.get(index) {
            output.push_str(&value.to_string());
            index += 1;
        } else {
            output.push_str("{}");
        }
        rest = &rest[position + 2..];
    }
    output.push_str(rest);
    Ok(Value::String(output))
}

fn option_value_or(name: &str, arguments: &[Value]) -> Result<Value, InterpError> {
    match arguments {
        [Value::Variant(variant, payload), default] if variant == "Some" => {
            Ok(payload.first().cloned().unwrap_or_else(|| default.clone()))
        }
        [Value::Variant(variant, _), default] if variant == "None" => Ok(default.clone()),
        _ => Err(builtin_type_error(name, "an Option and default value")),
    }
}

fn string_unary(
    name: &str,
    arguments: &[Value],
    apply: impl FnOnce(&str) -> String,
) -> Result<Value, InterpError> {
    match arguments {
        [Value::String(value)] => Ok(Value::String(apply(value))),
        _ => Err(builtin_type_error(name, "one string")),
    }
}

fn string_predicate(name: &str, arguments: &[Value]) -> Result<Value, InterpError> {
    match arguments {
        [Value::String(value), Value::String(argument)] => Ok(Value::Bool(match name {
            "str_contains" | "contains" => value.contains(argument),
            "str_starts_with" | "starts_with" => value.starts_with(argument),
            "str_ends_with" | "ends_with" => value.ends_with(argument),
            _ => false,
        })),
        _ => Err(builtin_type_error(name, "two strings")),
    }
}

fn expect_arity(name: &str, arguments: &[Value], expected: usize) -> Result<(), InterpError> {
    if arguments.len() == expected {
        Ok(())
    } else {
        Err(InterpError::wrong_arg_count(format!(
            "builtin '{name}' expects {expected} arguments, got {}",
            arguments.len()
        )))
    }
}

fn builtin_type_error(name: &str, expected: &str) -> InterpError {
    InterpError::type_mismatch(format!("builtin '{name}' expects {expected}"))
}

fn bind_pattern(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    pattern: &ResolvedPattern,
    value: Value,
) -> Result<(), InterpError> {
    if try_bind_pattern(program, state, pattern, value)? {
        Ok(())
    } else {
        Err(InterpError::new(format!(
            "resolved pattern '{}' does not match its checked value",
            pattern.node_id.0
        )))
    }
}

fn try_bind_pattern(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    pattern: &ResolvedPattern,
    value: Value,
) -> Result<bool, InterpError> {
    let previous = current_frame(state)?.values.clone();
    let matched = match_pattern(program, state, pattern, value)?;
    if !matched {
        current_frame(state)?.values = previous;
    }
    Ok(matched)
}

fn match_pattern(
    program: &CheckedProgram,
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
            match_pattern_list(program, state, patterns, values)
        }
        ResolvedPatternKind::Constructor { variant, fields } => {
            let values = match value {
                Value::Variant(identity, values)
                    if identity == variant.0
                        || program.resolved_member_name(variant) == Some(identity.as_str()) =>
                {
                    values
                }
                Value::Newtype(identity, value) if identity == variant.0 => vec![*value],
                _ => return Ok(false),
            };
            let patterns = fields
                .iter()
                .map(|(_, pattern)| pattern.clone())
                .collect::<Vec<_>>();
            match_pattern_list(program, state, &patterns, values)
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
                if !match_pattern(program, state, pattern, value)? {
                    return Ok(false);
                }
            }
            if let Some(rest) = rest {
                match_pattern(
                    program,
                    state,
                    rest,
                    Value::List(values[prefix.len()..].to_vec()),
                )
            } else {
                Ok(true)
            }
        }
    }
}

fn match_pattern_list(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    patterns: &[ResolvedPattern],
    values: Vec<Value>,
) -> Result<bool, InterpError> {
    if patterns.len() != values.len() {
        return Ok(false);
    }
    for (pattern, value) in patterns.iter().zip(values) {
        if !match_pattern(program, state, pattern, value)? {
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
                        let Some(input) = eval_child(program, state, body, input)? else {
                            return Ok(Vec::new());
                        };
                        match input {
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
            let Some(index_value) = eval_child(program, state, body, index)? else {
                return Ok(Value::Unit);
            };
            let index = match index_value {
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
        ResolvedValueProjection::Field(field) => {
            let name = program
                .resolved_member_name(field)
                .ok_or_else(|| unsupported(field, "rvalue field has no resolved display name"))?;
            project_value(value, &RuntimeProjection::Field(name.to_string()))
        }
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
        CheckedConversionKind::OwnershipWrap => {
            match program.resolved_types().get(&conversion.to) {
                Some(ResolvedType::Ownership {
                    kind: crate::core::OwnershipTypeKind::Shared,
                    ..
                }) => Ok(Value::Shared(std::sync::Arc::new(std::sync::RwLock::new(
                    value,
                )))),
                Some(ResolvedType::Ownership {
                    kind: crate::core::OwnershipTypeKind::LocalShared,
                    ..
                }) => Ok(Value::LocalShared(LocalSharedInner::new(value))),
                _ => Err(InterpError::type_mismatch(
                    "ownership wrap target is not shared/local_shared",
                )),
            }
        }
        CheckedConversionKind::OwnershipDowngrade => match value {
            Value::Shared(value) => Ok(Value::WeakShared(std::sync::Arc::downgrade(&value))),
            Value::LocalShared(value) => Ok(Value::WeakLocal(value.downgrade())),
            _ => Err(InterpError::type_mismatch(
                "ownership downgrade source is not strong ownership",
            )),
        },
        CheckedConversionKind::OwnershipRead => match value {
            Value::Shared(value) | Value::Ref(value) | Value::RefMut(value) => value
                .read()
                .map_err(|_| InterpError::lock_error("poisoned typed ownership value"))
                .map(|value| value.clone()),
            Value::LocalShared(value) => value
                .lock()
                .map_err(|_| InterpError::lock_error("poisoned typed local ownership value"))
                .map(|value| value.clone()),
            _ => Err(InterpError::type_mismatch(
                "ownership read source is not strong ownership",
            )),
        },
        CheckedConversionKind::DynamicPack | CheckedConversionKind::DynamicDowncastChecked => {
            Err(InterpError::new(format!(
                "conversion {:?} is outside the typed scalar execution subset",
                conversion.kind
            )))
        }
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

fn eval_child(
    program: &CheckedProgram,
    state: &mut ExecutionState,
    body: &ResolvedBody,
    expression: &ResolvedExpr,
) -> Result<Option<Value>, InterpError> {
    let value = eval_expr(program, state, body, expression)?;
    if signal_pending(state)? {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn signal_pending(state: &mut ExecutionState) -> Result<bool, InterpError> {
    Ok(current_frame(state)?.signal.is_some())
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
    use std::path::PathBuf;

    fn checked(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex typed interpreter fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse typed interpreter fixture");
        crate::core::check_program(&file).expect("check typed interpreter fixture")
    }

    fn checked_real_world(name: &str) -> CheckedProgram {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entry = root.join("tests/real_world").join(name);
        let mut loader = crate::loader::ModuleLoader::new(
            entry
                .parent()
                .expect("real-world fixture parent")
                .to_path_buf(),
        );
        loader
            .load_main(&entry)
            .unwrap_or_else(|error| panic!("load typed fixture '{name}': {error}"));
        let mut file = loader
            .merge_all()
            .unwrap_or_else(|error| panic!("merge typed fixture '{name}': {error}"));
        crate::loader::merge_prelude_into(&mut file);
        crate::core::check_program(&file)
            .unwrap_or_else(|diagnostics| panic!("check typed fixture '{name}': {diagnostics:#?}"))
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
        let program = checked("func main() -> string { to_json(1) }");
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
        assert!(error.message().contains("builtin 'to_json'"));
    }

    #[test]
    fn typed_constants_defaults_records_builtins_and_output_are_owned() {
        let program = checked(
            "const OFFSET: i32 = 2;\ntype Point { x: i32, y: i32 }\nfunc shift(value: i32, amount: i32 = OFFSET) -> i32 { value + amount }\nfunc main() -> i32 { let point = Point { x: shift(3), y: 4 }; println(point.x); len([point.x, point.y]) + point.x }",
        );
        let mut interpreter = ResolvedInterpreter::new(&program);
        assert_eq!(interpreter.run_main().unwrap(), Value::Int(7));
        assert_eq!(interpreter.output(), "5\n");
    }

    #[test]
    fn typed_sparse_named_defaults_use_callee_parameter_identities() {
        let program = checked(
            "func combine(left: i32 = 1, middle: i32 = 2, right: i32 = 3) -> i32 { left * 100 + middle * 10 + right }\nfunc main() -> i32 { combine(right = 9, left = 4) }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(429)
        );
    }

    #[test]
    fn typed_nested_callable_reads_owned_capture_identities() {
        let program = checked(
            "func outer(base: i32) -> i32 { func add(value: i32) -> i32 { base + value } add(3) }\nfunc main() -> i32 { outer(7) }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(10)
        );
    }

    #[test]
    fn typed_option_constructors_and_methods_use_canonical_builtin_ids() {
        let program = checked(
            "func select(value: Option<i32>) -> i32 { if value.is_some() { value.unwrap_or(0) } else { 0 } }\nfunc main() -> i32 { select(Some(8)) }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(8)
        );
    }

    #[test]
    fn typed_lambda_executes_with_owned_capture_environment() {
        let program = checked(
            "func main() -> i32 { let base = 6; let multiply = fn(value: i32) -> i32 { let adjusted = value + 1; adjusted * base }; multiply(4) }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(30)
        );
    }

    #[test]
    fn typed_first_class_function_dispatches_by_closed_callable_identity() {
        let program = checked(
            "func increment(value: i32) -> i32 { value + 1 }\nfunc main() -> i32 { let callable = increment; callable(8) }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(9)
        );
    }

    #[test]
    fn typed_adt_patterns_compare_canonical_variant_identities() {
        let program = checked(
            "type Choice { Pick(i32) Empty }\nfunc choose(value: Choice) -> i32 { match value { Pick(inner) => inner, Empty => 0 } }\nfunc option(value: Option<i32>) -> i32 { match value { Some(inner) => inner, None => 0 } }\nfunc main() -> i32 { choose(Pick(9)) + option(Some(4)) }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(13)
        );
    }

    #[test]
    fn typed_try_propagation_survives_nested_parent_expressions() {
        let program = checked(
            "type Res { Ok(i32) Err(string) }\nfunc increment(value: Res) -> Res { Ok(value? + 1) }\nfunc code(value: Res) -> i32 { match increment(value) { Ok(inner) => inner, Err(_) => 9 } }\nfunc main() -> i32 { code(Ok(4)) * 10 + code(Err(\"stop\")) }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(59)
        );
    }

    #[test]
    fn typed_higher_order_builtins_accept_callable_parameters() {
        let program = checked(
            "func transform(values: List<i32>, mapper: func(i32) -> i32) -> List<i32> { map(values, mapper) }\nfunc main() -> i32 { let mapped = transform([1, 2, 3], fn(value: i32) -> i32 { value * 3 }); let even = filter(mapped, fn(value: i32) -> bool { value % 2 == 0 }); reduce(mapped, fn(total: i32, value: i32) -> i32 { total + value }, 0) + len(even) }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(19)
        );
    }

    #[test]
    fn typed_option_map_invokes_canonical_callable_argument() {
        let program = checked(
            "func main() -> i32 { let mapped = Some(4).map(fn(value: i32) -> i32 { value + 2 }); match mapped { Some(value) => value, None => 0 } }",
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(6)
        );
    }

    #[test]
    fn typed_real_world_core_value_suite_runs_without_surface_ast() {
        let fixtures = [
            (
                "core_basic_control",
                include_str!("../../tests/real_world/core_basic_control.mimi"),
            ),
            (
                "core_closures",
                include_str!("../../tests/real_world/core_closures.mimi"),
            ),
            (
                "core_enums_match",
                include_str!("../../tests/real_world/core_enums_match.mimi"),
            ),
            (
                "core_functions_recursion",
                include_str!("../../tests/real_world/core_functions_recursion.mimi"),
            ),
            (
                "core_generics_adt",
                include_str!("../../tests/real_world/core_generics_adt.mimi"),
            ),
            (
                "core_list_index",
                include_str!("../../tests/real_world/core_list_index.mimi"),
            ),
            (
                "core_newtype",
                include_str!("../../tests/real_world/core_newtype.mimi"),
            ),
            (
                "core_option_result",
                include_str!("../../tests/real_world/core_option_result.mimi"),
            ),
            (
                "core_records",
                include_str!("../../tests/real_world/core_records.mimi"),
            ),
            (
                "core_try_operator",
                include_str!("../../tests/real_world/core_try_operator.mimi"),
            ),
            (
                "core_traits_methods",
                include_str!("../../tests/real_world/core_traits_methods.mimi"),
            ),
            (
                "core_shared_weak",
                include_str!("../../tests/real_world/core_shared_weak.mimi"),
            ),
        ];
        for (name, source) in fixtures {
            let program = checked(source);
            let value = ResolvedInterpreter::new(&program)
                .run_main()
                .unwrap_or_else(|error| panic!("typed fixture '{name}' failed: {error}"));
            assert_eq!(value, Value::Int(0), "typed fixture '{name}'");
        }
    }

    #[test]
    fn typed_real_world_pure_stdlib_suite_runs_from_merged_modules() {
        // TOOL-RESOLUTION-001: imported stdlib bodies execute only from the
        // checker-owned call, type, local, and place identities.
        for name in [
            "std_collections.mimi",
            "std_csv.mimi",
            "std_maps.mimi",
            "std_mymath.mimi",
            "std_prelude.mimi",
            "std_set.mimi",
            "std_strings.mimi",
            "std_template.mimi",
        ] {
            let program = checked_real_world(name);
            let value = ResolvedInterpreter::new(&program)
                .run_main()
                .unwrap_or_else(|error| panic!("typed fixture '{name}' failed: {error}"));
            assert_eq!(value, Value::Int(0), "typed fixture '{name}'");
        }
    }

    #[test]
    fn typed_value_builtins_preserve_container_and_unicode_semantics() {
        let program = checked(
            r#"
            func main() -> i32 {
                let mut items = [3, 1]
                push(items, 2)
                let sorted = sort(items)
                let reversed = reverse(sorted)
                let flat = flatten([[1, 2], [3]])
                let pairs = zip(sorted, [4, 5, 6])
                let indexed = enumerate(sorted)
                let map = map_from_list([("a", 1), ("b", 2)])
                let map2 = map_remove(map_set(map, "c", 3), "a")
                let lookup = map_get(map2, "c")
                let position = str_index_of("aéz", "é")
                if len(items) == 3
                    && sorted[0] == 1
                    && reversed[0] == 3
                    && len(flat) == 3
                    && pairs[0].0 == 1
                    && indexed[2].0 == 2
                    && sum(sorted) == 6
                    && lookup.0
                    && map_size(map2) == 2
                    && has_key(map2, "b")
                    && len(keys(map2)) == 2
                    && len(values(map2)) == 2
                    && str_char_at("aéz", 1) == "é"
                    && str_substring("aéz", 1, 3) == "éz"
                    && str_replace("aba", "a", "x") == "xbx"
                    && str_repeat("é", 2) == "éé"
                    && position.unwrap_or(-1) == 1
                    && chr(char_code("é", 0)) == "é"
                    && pop(items) == 2
                { 0 } else { 1 }
            }
            "#,
        );
        assert_eq!(
            ResolvedInterpreter::new(&program).run_main().unwrap(),
            Value::Int(0)
        );
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
