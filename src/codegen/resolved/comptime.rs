//! Constant evaluation over checker-owned Typed Resolved IR.
//!
//! This is intentionally independent of the retained AST compatibility
//! snapshot.  It covers the scalar value family accepted by the native
//! comptime ABI today and fails with a source-facing comptime error when a
//! required input depends on runtime state or an unsupported operation.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{ResolvedBinaryOp, ResolvedUnaryOp};
use crate::core::{
    CheckedConversion, CheckedProgram, NodeId, ResolvedBlock, ResolvedBody, ResolvedCallee,
    ResolvedConstValue, ResolvedExpr, ResolvedExprKind, ResolvedLiteral, ResolvedLocalId,
    ResolvedPatternKind, ResolvedPlace, ResolvedStmtKind, ResolvedType,
};
use crate::interp::{SharedString, Value};

pub(super) type ComptimeValues = BTreeMap<NodeId, Value>;

enum FoldOutcome<T> {
    Value(T),
    Return(Value),
}

macro_rules! propagate_return {
    ($expression:expr) => {
        match $expression? {
            FoldOutcome::Value(value) => value,
            FoldOutcome::Return(value) => return Ok(FoldOutcome::Return(value)),
        }
    };
}

pub(super) fn fold_comptime_items(program: &CheckedProgram) -> (ComptimeValues, Option<String>) {
    let empty = ComptimeValues::new();
    let mut folder = TypedComptimeFolder::new(program, &empty);
    let mut functions = program
        .functions()
        .values()
        .filter(|function| function.is_comptime && function.params.is_empty())
        .collect::<Vec<_>>();
    functions.sort_by_key(|function| {
        let span = function.origin.user_span();
        (
            span.source_id.raw(),
            span.start_line,
            span.start_col,
            function.node_id.clone(),
        )
    });

    let mut values = ComptimeValues::new();
    for function in functions {
        match folder.eval_function(&function.node_id, &[], false) {
            Ok(value) => {
                values.insert(function.node_id.clone(), value);
            }
            Err(error) => return (ComptimeValues::new(), Some(error)),
        }
    }
    (values, None)
}

pub(super) struct TypedComptimeFolder<'program, 'cache> {
    program: &'program CheckedProgram,
    cached_values: &'cache ComptimeValues,
    active_constants: BTreeSet<NodeId>,
    depth: usize,
    ieee_depth: usize,
    contracts_enabled: bool,
}

impl<'program, 'cache> TypedComptimeFolder<'program, 'cache> {
    pub(super) fn new(
        program: &'program CheckedProgram,
        cached_values: &'cache ComptimeValues,
    ) -> Self {
        Self {
            program,
            cached_values,
            active_constants: BTreeSet::new(),
            depth: 0,
            ieee_depth: 0,
            contracts_enabled: true,
        }
    }

    pub(super) fn eval_comptime_block(
        &mut self,
        body: &ResolvedBody,
        block: &ResolvedBlock,
    ) -> Result<Value, String> {
        // Inline `comptime {}` historically runs its synthetic VM wrapper
        // with contract verification disabled. Top-level items use a fresh
        // folder and retain the default-on contract policy below.
        let caller_contracts_enabled = self.contracts_enabled;
        self.contracts_enabled = false;
        let result = self.with_depth(|folder| folder.eval_block(body, block, &mut BTreeMap::new()));
        self.contracts_enabled = caller_contracts_enabled;
        match result? {
            FoldOutcome::Value(value) | FoldOutcome::Return(value) => Ok(value),
        }
    }

    fn with_depth<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.depth >= 768 {
            return Err("comptime evaluation exceeded the maximum call depth".into());
        }
        self.depth += 1;
        let result = operation(self);
        self.depth -= 1;
        result
    }

    fn eval_function(
        &mut self,
        owner: &NodeId,
        arguments: &[Value],
        allow_pure: bool,
    ) -> Result<Value, String> {
        if arguments.is_empty() {
            if let Some(value) = self.cached_values.get(owner) {
                return Ok(value.clone());
            }
        }
        let function = self
            .program
            .functions()
            .get(owner)
            .ok_or_else(|| format!("comptime function identity '{}' is absent", owner.0))?;
        if !function.is_comptime && (!allow_pure || !function.effects.is_empty()) {
            return Err(format!(
                "comptime evaluation cannot call non-comptime or effectful function '{}'",
                function.qualified_name
            ));
        }
        let callable = self.program.callable(owner).ok_or_else(|| {
            format!(
                "typed body for comptime function '{}' is absent",
                function.qualified_name
            )
        })?;
        if callable.signature.parameters.len() != arguments.len() {
            return Err(format!(
                "comptime function '{}' expects {} arguments, got {}",
                function.qualified_name,
                callable.signature.parameters.len(),
                arguments.len()
            ));
        }

        let caller_ieee_depth = self.ieee_depth;
        self.ieee_depth = 0;
        let result = self.with_depth(|folder| {
            let mut locals = BTreeMap::new();
            for (parameter, value) in callable.body.parameters.iter().zip(arguments) {
                locals.insert(parameter.clone(), value.clone());
            }
            if folder.contracts_enabled {
                folder.check_requires(&callable.body, &function.qualified_name, &mut locals)?;
            }
            let value = match folder.eval_block(&callable.body, &callable.body.root, &mut locals)? {
                FoldOutcome::Value(value) | FoldOutcome::Return(value) => value,
            };
            if folder.contracts_enabled {
                folder.check_ensures(&callable.body, &function.qualified_name, &value, &locals)?;
            }
            Ok(value)
        });
        self.ieee_depth = caller_ieee_depth;
        result
    }

    fn check_requires(
        &mut self,
        body: &ResolvedBody,
        function_name: &str,
        locals: &mut BTreeMap<ResolvedLocalId, Value>,
    ) -> Result<(), String> {
        for statement in &body.root.statements {
            let ResolvedStmtKind::Contract {
                kind: crate::core::ir::ContractKind::Requires,
                condition,
            } = &statement.kind
            else {
                continue;
            };
            if !self.eval_bool(body, condition, locals)? {
                return Err(format!(
                    "[E0808] requires condition failed for '{function_name}': false"
                ));
            }
        }
        Ok(())
    }

    fn check_ensures(
        &mut self,
        body: &ResolvedBody,
        function_name: &str,
        value: &Value,
        locals: &BTreeMap<ResolvedLocalId, Value>,
    ) -> Result<(), String> {
        let result_local = body
            .locals
            .values()
            .find(|local| local.display_name == "result")
            .map(|local| local.id.clone());
        let mut post_locals = locals.clone();
        if let Some(result_local) = result_local {
            post_locals.insert(result_local, value.clone());
        }
        for statement in &body.root.statements {
            let ResolvedStmtKind::Contract {
                kind: crate::core::ir::ContractKind::Ensures,
                condition,
            } = &statement.kind
            else {
                continue;
            };
            if !self.eval_bool(body, condition, &mut post_locals)? {
                return Err(format!(
                    "[E0808] ensures condition failed for '{function_name}': false"
                ));
            }
        }
        Ok(())
    }

    fn eval_block(
        &mut self,
        body: &ResolvedBody,
        block: &ResolvedBlock,
        locals: &mut BTreeMap<ResolvedLocalId, Value>,
    ) -> Result<FoldOutcome<Value>, String> {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    if let Some(initializer) = initializer {
                        let value = propagate_return!(self.eval_expr(body, initializer, locals));
                        self.bind_pattern(pattern, value, locals)?;
                    }
                }
                ResolvedStmtKind::Assign {
                    target,
                    value,
                    conversion,
                } => {
                    let value = propagate_return!(self.eval_expr(body, value, locals));
                    let value = eval_cast(value, conversion, self.program)?;
                    self.store_place(target, value, locals)?;
                }
                ResolvedStmtKind::Return { value, conversion } => {
                    let value = match value {
                        Some(value) => propagate_return!(self.eval_expr(body, value, locals)),
                        None => Value::Unit,
                    };
                    let value = match conversion {
                        Some(conversion) => eval_cast(value, conversion, self.program)?,
                        None => value,
                    };
                    return Ok(FoldOutcome::Return(value));
                }
                ResolvedStmtKind::Expr(expression) => {
                    match self.eval_expr(body, expression, locals)? {
                        FoldOutcome::Value(_) => {}
                        FoldOutcome::Return(value) => {
                            return Ok(FoldOutcome::Return(value));
                        }
                    }
                }
                ResolvedStmtKind::Contract { .. } => {}
                ResolvedStmtKind::Scope { kind, body: nested } => match kind {
                    crate::core::ir::ResolvedScopeKind::Lexical
                    | crate::core::ir::ResolvedScopeKind::Unsafe => {
                        match self.eval_block(body, nested, locals)? {
                            FoldOutcome::Value(_) => {}
                            FoldOutcome::Return(value) => {
                                return Ok(FoldOutcome::Return(value));
                            }
                        }
                    }
                    crate::core::ir::ResolvedScopeKind::IeeeFloat => {
                        self.ieee_depth += 1;
                        let result = self.eval_block(body, nested, locals);
                        self.ieee_depth -= 1;
                        match result? {
                            FoldOutcome::Value(_) => {}
                            FoldOutcome::Return(value) => {
                                return Ok(FoldOutcome::Return(value));
                            }
                        }
                    }
                    unsupported => {
                        return Err(format!(
                            "comptime scope {unsupported:?} is outside the typed scalar fold slice"
                        ));
                    }
                },
                other => {
                    return Err(format!(
                        "comptime statement {:?} is outside the typed scalar fold slice",
                        std::mem::discriminant(other)
                    ));
                }
            }
        }
        match block.result.as_deref() {
            Some(expression) => self.eval_expr(body, expression, locals),
            None => Ok(FoldOutcome::Value(Value::Unit)),
        }
    }

    fn bind_pattern(
        &self,
        pattern: &crate::core::ResolvedPattern,
        value: Value,
        locals: &mut BTreeMap<ResolvedLocalId, Value>,
    ) -> Result<(), String> {
        match &pattern.kind {
            ResolvedPatternKind::Binding { local, .. } => {
                locals.insert(local.clone(), value);
                Ok(())
            }
            ResolvedPatternKind::Wildcard => Ok(()),
            _ => Err("comptime scalar fold only supports binding and wildcard patterns".into()),
        }
    }

    fn store_place(
        &self,
        place: &ResolvedPlace,
        value: Value,
        locals: &mut BTreeMap<ResolvedLocalId, Value>,
    ) -> Result<(), String> {
        if !place.projections.is_empty() {
            return Err("comptime scalar fold cannot assign through a projection".into());
        }
        if !locals.contains_key(&place.base) {
            return Err("comptime assignment depends on a runtime local".into());
        }
        locals.insert(place.base.clone(), value);
        Ok(())
    }

    fn eval_expr(
        &mut self,
        body: &ResolvedBody,
        expression: &ResolvedExpr,
        locals: &mut BTreeMap<ResolvedLocalId, Value>,
    ) -> Result<FoldOutcome<Value>, String> {
        match &expression.kind {
            ResolvedExprKind::Literal(literal) => Ok(FoldOutcome::Value(match literal {
                ResolvedLiteral::Int(value) => Value::Int(*value),
                ResolvedLiteral::FloatBits(value) => Value::Float(f64::from_bits(*value)),
                ResolvedLiteral::Bool(value) => Value::Bool(*value),
                ResolvedLiteral::String(value) => Value::String(SharedString::new(value.clone())),
                ResolvedLiteral::Unit => Value::Unit,
            })),
            ResolvedExprKind::Load(place) => {
                if !place.projections.is_empty() {
                    return Err("comptime load depends on an unsupported projection".into());
                }
                locals
                    .get(&place.base)
                    .cloned()
                    .map(FoldOutcome::Value)
                    .ok_or_else(|| "comptime expression depends on a runtime local".into())
            }
            ResolvedExprKind::Constant(owner) => self.eval_constant(owner).map(FoldOutcome::Value),
            ResolvedExprKind::Binary { op, left, right } => {
                let left = propagate_return!(self.eval_expr(body, left, locals));
                if *op == ResolvedBinaryOp::LogicalAnd && !truthy(&left)? {
                    return Ok(FoldOutcome::Value(Value::Bool(false)));
                }
                if *op == ResolvedBinaryOp::LogicalOr && truthy(&left)? {
                    return Ok(FoldOutcome::Value(Value::Bool(true)));
                }
                let right = propagate_return!(self.eval_expr(body, right, locals));
                eval_binary(
                    *op,
                    left,
                    right,
                    &expression.ty,
                    self.program,
                    self.ieee_depth > 0,
                )
                .map(FoldOutcome::Value)
            }
            ResolvedExprKind::Unary { op, operand } => {
                let value = propagate_return!(self.eval_expr(body, operand, locals));
                eval_unary(*op, value, &expression.ty, self.program).map(FoldOutcome::Value)
            }
            ResolvedExprKind::Call(call) => {
                let ResolvedCallee::Function(owner) = &call.callee else {
                    return Err("comptime call is not a resolved pure function".into());
                };
                if !call.type_arguments.is_empty() {
                    return Err(
                        "comptime generic calls are outside the typed scalar fold slice".into(),
                    );
                }
                let mut arguments = Vec::with_capacity(call.arguments.len());
                for argument in &call.arguments {
                    let value = propagate_return!(self.eval_expr(body, &argument.value, locals));
                    arguments.push(eval_cast(value, &argument.conversion, self.program)?);
                }
                let function =
                    self.program.functions().get(owner).ok_or_else(|| {
                        format!("comptime function identity '{}' is absent", owner.0)
                    })?;
                self.eval_function(owner, &arguments, !function.is_comptime)
                    .map(FoldOutcome::Value)
            }
            ResolvedExprKind::Block(block) => self.eval_block(body, block, locals),
            ResolvedExprKind::Comptime(block) => self
                .eval_comptime_block(body, block)
                .map(FoldOutcome::Value),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let condition = propagate_return!(self.eval_expr(body, condition, locals));
                if truthy(&condition)? {
                    self.eval_block(body, then_block, locals)
                } else {
                    self.eval_block(body, else_block, locals)
                }
            }
            ResolvedExprKind::Scope { kind, body: block } => match kind {
                crate::core::ir::ResolvedScopeKind::Lexical
                | crate::core::ir::ResolvedScopeKind::Unsafe => {
                    self.eval_block(body, block, locals)
                }
                crate::core::ir::ResolvedScopeKind::IeeeFloat => {
                    self.ieee_depth += 1;
                    let result = self.eval_block(body, block, locals);
                    self.ieee_depth -= 1;
                    result
                }
                unsupported => Err(format!(
                    "comptime scope {unsupported:?} is outside the typed scalar fold slice"
                )),
            },
            ResolvedExprKind::Cast { value, conversion } => {
                let value = propagate_return!(self.eval_expr(body, value, locals));
                eval_cast(value, conversion, self.program).map(FoldOutcome::Value)
            }
            _ => Err(format!(
                "comptime expression {:?} is outside the typed scalar fold slice",
                std::mem::discriminant(&expression.kind)
            )),
        }
    }

    fn eval_bool(
        &mut self,
        body: &ResolvedBody,
        expression: &ResolvedExpr,
        locals: &mut BTreeMap<ResolvedLocalId, Value>,
    ) -> Result<bool, String> {
        match self.eval_expr(body, expression, locals)? {
            FoldOutcome::Value(Value::Bool(value)) => Ok(value),
            FoldOutcome::Value(other) => {
                Err(format!("comptime contract expected bool, got {other:?}"))
            }
            FoldOutcome::Return(_) => Err("comptime contract expression returned early".into()),
        }
    }

    fn eval_constant(&mut self, owner: &NodeId) -> Result<Value, String> {
        if !self.active_constants.insert(owner.clone()) {
            return Err(format!("cyclic comptime constant '{}'", owner.0));
        }
        let result = (|| {
            if let Some(initializer) = self
                .program
                .declaration_expression_bodies()
                .values()
                .find(|entry| {
                    entry.owner == *owner
                        && entry.kind
                            == crate::core::ir::lower::ResolvedDeclarationExpressionKind::ConstantInitializer
                })
            {
                let typed_body = initializer.body.as_ref().map_err(|errors| {
                    format!(
                        "typed initializer for constant '{}' is unavailable: {errors:?}",
                        owner.0
                    )
                })?;
                return match self.eval_block(
                    typed_body,
                    &typed_body.root,
                    &mut BTreeMap::new(),
                )? {
                    FoldOutcome::Value(value) => Ok(value),
                    FoldOutcome::Return(_) => {
                        Err("return in comptime constant initializer".into())
                    }
                };
            }
            if let Some(constant) = self.program.constants().get(owner) {
                let materialized = match &constant.value {
                    ResolvedConstValue::Int(value) => Some(Value::Int(*value)),
                    ResolvedConstValue::Float(value) => Some(Value::Float(*value)),
                    ResolvedConstValue::Bool(value) => Some(Value::Bool(*value)),
                    ResolvedConstValue::String(value) => {
                        Some(Value::String(SharedString::new(value.clone())))
                    }
                    ResolvedConstValue::Unit => Some(Value::Unit),
                    ResolvedConstValue::Complex => None,
                };
                if let Some(materialized) = materialized {
                    return Ok(materialized);
                }
            }
            Err(format!(
                "typed initializer for constant '{}' is absent",
                owner.0
            ))
        })();
        self.active_constants.remove(owner);
        result
    }
}

fn truthy(value: &Value) -> Result<bool, String> {
    match value {
        Value::Bool(value) => Ok(*value),
        _ => Err(format!("comptime condition expected bool, got {value:?}")),
    }
}

fn eval_unary(
    op: ResolvedUnaryOp,
    value: Value,
    result_type: &crate::core::ResolvedTypeId,
    program: &CheckedProgram,
) -> Result<Value, String> {
    match (op, value) {
        (ResolvedUnaryOp::Negate, Value::Int(value)) => {
            let value = value
                .checked_neg()
                .ok_or_else(|| "[E0802] comptime integer overflow".to_string())?;
            if is_i32(program, result_type) && i32::try_from(value).is_err() {
                return Err("[E0802] comptime i32 arithmetic overflow".into());
            }
            Ok(Value::Int(value))
        }
        (ResolvedUnaryOp::Negate, Value::Float(value)) => Ok(Value::Float(-value)),
        (ResolvedUnaryOp::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
        (ResolvedUnaryOp::BorrowShared | ResolvedUnaryOp::BorrowMutable, value)
        | (ResolvedUnaryOp::Dereference, value) => Ok(value),
        (op, value) => Err(format!(
            "comptime unary operation {op:?} does not accept {value:?}"
        )),
    }
}

fn eval_binary(
    op: ResolvedBinaryOp,
    left: Value,
    right: Value,
    result_type: &crate::core::ResolvedTypeId,
    program: &CheckedProgram,
    ieee_float: bool,
) -> Result<Value, String> {
    use ResolvedBinaryOp as Op;
    match (left, right) {
        (Value::Int(left), Value::Int(right)) => {
            let comparison = match op {
                Op::Equal => Some(left == right),
                Op::NotEqual => Some(left != right),
                Op::Less => Some(left < right),
                Op::Greater => Some(left > right),
                Op::LessEqual => Some(left <= right),
                Op::GreaterEqual => Some(left >= right),
                _ => None,
            };
            if let Some(value) = comparison {
                return Ok(Value::Bool(value));
            }
            let value = match op {
                Op::Add => left.checked_add(right),
                Op::Subtract => left.checked_sub(right),
                Op::Multiply => left.checked_mul(right),
                Op::Divide if right == 0 => return Err("[E0801] comptime division by zero".into()),
                Op::Divide
                    if is_i32(program, result_type) && left == i32::MIN as i64 && right == -1 =>
                {
                    return Err("[E0802] comptime i32 division overflow".into())
                }
                Op::Divide => left.checked_div(right),
                Op::Remainder if right == 0 => {
                    return Err("[E0801] comptime remainder by zero".into())
                }
                Op::Remainder
                    if is_i32(program, result_type) && left == i32::MIN as i64 && right == -1 =>
                {
                    return Err("[E0802] comptime i32 remainder overflow".into())
                }
                Op::Remainder => left.checked_rem(right),
                Op::Power => {
                    let power = u32::try_from(right)
                        .ok()
                        .and_then(|exponent| left.checked_pow(exponent));
                    if is_i32(program, result_type) {
                        power.map(|value| value as i32 as i64)
                    } else {
                        power
                    }
                }
                Op::BitAnd => Some(left & right),
                Op::BitOr => Some(left | right),
                Op::BitXor => Some(left ^ right),
                Op::ShiftLeft if is_i32(program, result_type) => {
                    Some(((left as i32).wrapping_shl((right as u32) & 31)) as i64)
                }
                Op::ShiftLeft => Some(left.wrapping_shl((right as u32) & 63)),
                Op::ShiftRight if is_i32(program, result_type) => {
                    Some(((left as i32) >> ((right as u32) & 31)) as i64)
                }
                Op::ShiftRight => Some(left >> ((right as u32) & 63)),
                Op::LogicalAnd | Op::LogicalOr => {
                    return Err("comptime logical operation requires bool operands".into())
                }
                Op::Equal
                | Op::NotEqual
                | Op::Less
                | Op::Greater
                | Op::LessEqual
                | Op::GreaterEqual => unreachable!(),
            }
            .ok_or_else(|| "[E0802] comptime integer overflow or invalid shift".to_string())?;
            if is_i32(program, result_type) && i32::try_from(value).is_err() {
                return Err("[E0802] comptime i32 arithmetic overflow".into());
            }
            Ok(Value::Int(value))
        }
        (Value::Float(left), Value::Float(right)) => eval_float_binary(op, left, right, ieee_float),
        (Value::Int(left), Value::Float(right)) => {
            eval_float_binary(op, left as f64, right, ieee_float)
        }
        (Value::Float(left), Value::Int(right)) => {
            eval_float_binary(op, left, right as f64, ieee_float)
        }
        (Value::Bool(left), Value::Bool(right)) => match op {
            Op::LogicalAnd => Ok(Value::Bool(left && right)),
            Op::LogicalOr => Ok(Value::Bool(left || right)),
            Op::Equal => Ok(Value::Bool(left == right)),
            Op::NotEqual => Ok(Value::Bool(left != right)),
            _ => Err(format!("comptime bool operation {op:?} is unsupported")),
        },
        (Value::String(left), Value::String(right)) if op == Op::Add => {
            Ok(Value::String(SharedString::new(format!("{left}{right}"))))
        }
        (left, right) if matches!(op, Op::Equal | Op::NotEqual) => {
            let equal = crate::interp::values_equal(&left, &right);
            Ok(Value::Bool(if op == Op::Equal { equal } else { !equal }))
        }
        (left, right) => Err(format!(
            "comptime operation {op:?} does not accept {left:?} and {right:?}"
        )),
    }
}

fn is_i32(program: &CheckedProgram, ty: &crate::core::ResolvedTypeId) -> bool {
    matches!(
        program.resolved_types().get(ty),
        Some(ResolvedType::Primitive(crate::core::PrimitiveType::I32))
    )
}

fn eval_float_binary(
    op: ResolvedBinaryOp,
    left: f64,
    right: f64,
    ieee_float: bool,
) -> Result<Value, String> {
    use ResolvedBinaryOp as Op;
    let comparison = match op {
        Op::Equal => Some(left == right),
        Op::NotEqual => Some(left != right),
        Op::Less => Some(left < right),
        Op::Greater => Some(left > right),
        Op::LessEqual => Some(left <= right),
        Op::GreaterEqual => Some(left >= right),
        _ => None,
    };
    if let Some(value) = comparison {
        return Ok(Value::Bool(value));
    }
    let value = match op {
        Op::Add => left + right,
        Op::Subtract => left - right,
        Op::Multiply => left * right,
        Op::Divide if right == 0.0 && !ieee_float => {
            return Err("[E0801] comptime division by zero".into())
        }
        Op::Divide => left / right,
        Op::Remainder if right == 0.0 && !ieee_float => {
            return Err("[E0801] comptime remainder by zero".into())
        }
        Op::Remainder => left % right,
        Op::Power => left.powf(right),
        _ => return Err(format!("comptime float operation {op:?} is unsupported")),
    };
    finite(value, ieee_float)
}

fn finite(value: f64, ieee_float: bool) -> Result<Value, String> {
    if ieee_float || value.is_finite() {
        Ok(Value::Float(value))
    } else {
        Err("[E0813] comptime floating-point result is not finite".into())
    }
}

fn eval_cast(
    value: Value,
    conversion: &CheckedConversion,
    program: &CheckedProgram,
) -> Result<Value, String> {
    use crate::core::{CheckedConversionKind, PrimitiveType};

    match conversion.kind {
        CheckedConversionKind::Identity
        | CheckedConversionKind::AliasWrap
        | CheckedConversionKind::AliasUnwrap
        | CheckedConversionKind::NewtypeWrap
        | CheckedConversionKind::NewtypeUnwrap
        | CheckedConversionKind::OwnershipWrap
        | CheckedConversionKind::OwnershipDowngrade
        | CheckedConversionKind::OwnershipRead => return Ok(value),
        CheckedConversionKind::NumericWiden | CheckedConversionKind::NumericNarrowChecked => {}
        kind => {
            return Err(format!(
                "comptime cast conversion {kind:?} is outside the typed scalar fold slice"
            ));
        }
    }

    let target = program.resolved_types().get(&conversion.to);
    match (target, value) {
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::I32)), Value::Int(value)) => {
            Ok(Value::Int(value as i32 as i64))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::I32)), Value::Float(value)) => {
            // Mimi's explicit float-to-integer casts follow Rust's saturating
            // float cast rule: NaN becomes zero and infinities clamp to the
            // destination bounds.
            Ok(Value::Int(value as i32 as i64))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::I32)), Value::Bool(value)) => {
            Ok(Value::Int(i64::from(value)))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::I64)), Value::Int(value)) => {
            Ok(Value::Int(value))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::I64)), Value::Float(value)) => {
            Ok(Value::Int(value as i64))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::I64)), Value::Bool(value)) => {
            Ok(Value::Int(i64::from(value)))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::F64)), Value::Int(value)) => {
            Ok(Value::Float(value as f64))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::F64)), Value::Float(value)) => {
            Ok(Value::Float(value))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::F32)), Value::Int(value)) => {
            Ok(Value::Float((value as f32) as f64))
        }
        (Some(crate::core::ResolvedType::Primitive(PrimitiveType::F32)), Value::Float(value)) => {
            Ok(Value::Float((value as f32) as f64))
        }
        (Some(crate::core::ResolvedType::Primitive(primitive)), value) => Err(format!(
            "comptime cast to '{}' does not accept {value:?}",
            primitive.language_name()
        )),
        (target, value) => Err(format!(
            "comptime cast target {target:?} is outside the typed scalar fold slice (value {value:?})"
        )),
    }
}
