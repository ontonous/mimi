//! Direct execution of checker-owned typed bodies.
//!
//! This module is the replacement execution core for the surface-AST
//! interpreter. It deliberately accepts only `CheckedProgram` identities and
//! `ResolvedBody` nodes; unsupported typed constructs fail closed instead of
//! consulting the compatibility surface program.

use super::{is_truthy, ops, values_equal, InterpError, Value};
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
            arguments.into_iter().map(Some).collect(),
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
    arguments: Vec<Option<Value>>,
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
    for (local, value) in body.parameters.iter().zip(arguments) {
        if let Some(value) = value {
            values.insert(local.clone(), value);
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
            bind_pattern(program, state, pattern, value)?;
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
        ResolvedExprKind::Constant(identity) => constant_value(program, identity),
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
                    arguments.push(None);
                } else {
                    let value = eval_expr(program, state, body, &argument.value)?;
                    arguments.push(Some(apply_conversion(
                        program,
                        &argument.conversion,
                        value,
                    )?));
                }
            }
            match &call.callee {
                ResolvedCallee::Function(owner) => execute_call(program, state, owner, arguments),
                ResolvedCallee::Constructor(owner) => {
                    let arguments = require_complete_arguments(&expression.node_id, arguments)?;
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
                ResolvedCallee::Builtin(builtin) => eval_builtin(
                    state,
                    &expression.node_id,
                    builtin.as_str(),
                    require_complete_arguments(&expression.node_id, arguments)?,
                ),
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
                        require_complete_arguments(&expression.node_id, arguments)?,
                    )
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
        ResolvedExprKind::Map(entries) => {
            let mut fields = HashMap::with_capacity(entries.len());
            for (key, value) in entries {
                let key = eval_expr(program, state, body, key)?;
                let Value::String(key) = key else {
                    return Err(unsupported(
                        &expression.node_id,
                        "typed map keys are not strings",
                    ));
                };
                let value = eval_expr(program, state, body, value)?;
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
                if !try_bind_pattern(program, state, &arm.pattern, value.clone())? {
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
        ResolvedExprKind::Record { nominal, fields } => {
            let definition = program
                .type_defs()
                .get(&NodeId(nominal.as_str().to_string()));
            let mut values = HashMap::with_capacity(fields.len());
            for field in fields {
                let name = program.resolved_member_name(&field.field).ok_or_else(|| {
                    unsupported(&field.field, "record field has no resolved display name")
                })?;
                let value = eval_expr(program, state, body, &field.value)?;
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
            let iterable = eval_expr(program, state, body, iterable)?;
            let mut values = Vec::new();
            for item in iterable_values(&expression.node_id, iterable)? {
                let previous = current_frame(state)?.values.clone();
                if try_bind_pattern(program, state, pattern, item)? {
                    let selected = match guard {
                        Some(guard) => is_truthy(&eval_expr(program, state, body, guard)?),
                        None => true,
                    };
                    if selected {
                        values.push(eval_expr(program, state, body, value)?);
                    }
                }
                current_frame(state)?.values = previous;
            }
            Ok(Value::List(values))
        }
        ResolvedExprKind::OptionalChain {
            receiver, field, ..
        } => {
            let receiver = eval_expr(program, state, body, receiver)?;
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
            let target = eval_expr(program, state, body, target)?;
            let start = eval_optional_index(program, state, body, start.as_deref())?;
            let end = eval_optional_index(program, state, body, end.as_deref())?;
            slice_value(&expression.node_id, target, start, end)
        }
        ResolvedExprKind::TypeOf(_)
        | ResolvedExprKind::Try { .. }
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
    values
        .iter()
        .map(|value| eval_expr(program, state, body, value))
        .collect()
}

fn eval_callable(
    state: &mut ExecutionState,
    body: &ResolvedBody,
    expression: &ResolvedExpr,
) -> Result<Option<RuntimeCallable>, InterpError> {
    match &expression.kind {
        ResolvedExprKind::Callable(callee) => Ok(Some(RuntimeCallable::Direct(callee.clone()))),
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
    arguments: Vec<Value>,
) -> Result<Value, InterpError> {
    match callable {
        RuntimeCallable::Direct(ResolvedCallee::Function(owner)) => execute_call(
            program,
            state,
            &owner,
            arguments.into_iter().map(Some).collect(),
        ),
        RuntimeCallable::Direct(ResolvedCallee::Builtin(builtin)) => {
            eval_builtin(state, call_node, builtin.as_str(), arguments)
        }
        RuntimeCallable::Lambda(runtime) => {
            let RuntimeLambda {
                body_owner,
                lambda,
                mut captured_values,
                captured_callables,
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
                captured_values.insert(parameter.clone(), argument);
            }
            state.frames.push(Frame {
                owner: lambda.owner.clone(),
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

fn require_complete_arguments(
    node: &NodeId,
    arguments: Vec<Option<Value>>,
) -> Result<Vec<Value>, InterpError> {
    arguments
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            value.ok_or_else(|| {
                unsupported(
                    node,
                    &format!("non-function callee has a default argument at slot {index}"),
                )
            })
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
    expression
        .map(
            |expression| match eval_expr(program, state, body, expression)? {
                Value::Int(index) => usize::try_from(index)
                    .map(Some)
                    .map_err(|_| InterpError::index_out_of_bounds("negative slice bound")),
                _ => Err(unsupported(
                    &expression.node_id,
                    "slice bound is not an integer",
                )),
            },
        )
        .transpose()
        .map(Option::flatten)
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
        _ => Err(unsupported(
            node,
            &format!("Option/Result method '{method}' requires typed callable support"),
        )),
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
