use crate::core::{
    CheckedConversionKind, CheckedProgram, NodeId, PrimitiveType, ResolvedBlock, ResolvedCallable,
    ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedPattern, ResolvedPatternKind,
    ResolvedPlace, ResolvedStmtKind, ResolvedType, ResolvedTypeId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UnsupportedResolvedNode {
    pub owner: NodeId,
    pub node: NodeId,
    pub reason: String,
}

impl UnsupportedResolvedNode {
    fn new(owner: &NodeId, node: &NodeId, reason: impl Into<String>) -> Self {
        Self {
            owner: owner.clone(),
            node: node.clone(),
            reason: reason.into(),
        }
    }
}

pub(super) fn require_resolved_native_program(
    program: &CheckedProgram,
) -> Result<(), UnsupportedResolvedNode> {
    let user_flow_count = program
        .flows()
        .values()
        .filter(|flow| matches!(flow.origin, crate::core::Origin::User(_)))
        .count();
    if user_flow_count != 0
        || !program.actors().is_empty()
        || !program.sessions().is_empty()
        || !program.protocols().is_empty()
        || !program.capabilities().is_empty()
        || !program.traits().is_empty()
        || !program.impls().is_empty()
        || !program.extern_blocks().is_empty()
    {
        let owner = program
            .functions()
            .values()
            .next()
            .map(|function| function.node_id.clone())
            .unwrap_or_else(|| NodeId("resolved-native:program".into()));
        return Err(UnsupportedResolvedNode::new(
            &owner,
            &owner,
            format!(
                "declaration kinds beyond plain functions are not in the resolved native slice \
                 (flows={}, actors={}, sessions={}, protocols={}, caps={}, constants={}, traits={}, \
                 impls={}, types={}, externs={})",
                user_flow_count,
                program.actors().len(),
                program.sessions().len(),
                program.protocols().len(),
                program.capabilities().len(),
                program.constants().len(),
                program.traits().len(),
                program.impls().len(),
                program.type_defs().len(),
                program.extern_blocks().len(),
            ),
        ));
    }
    // Constants are allowed, but only materializable (non-Complex) values.
    for constant in program.constants().values() {
        if matches!(constant.value, crate::core::ResolvedConstValue::Complex) {
            return Err(UnsupportedResolvedNode::new(
                &constant.node_id,
                &constant.node_id,
                "constant with non-materializable value is not in the resolved native slice",
            ));
        }
    }
    for function in program.functions().values() {
        if function.is_comptime {
            continue;
        }
        if function.is_async || function.extern_abi.is_some() || !function.generics.is_empty() {
            return Err(UnsupportedResolvedNode::new(
                &function.node_id,
                &function.node_id,
                "async, export, and generic functions are not in the resolved native slice",
            ));
        }
        if function.qualified_name.contains("::") {
            return Err(UnsupportedResolvedNode::new(
                &function.node_id,
                &function.node_id,
                "qualified function symbols are not in the resolved native slice",
            ));
        }
        let callable = program.callable(&function.node_id).ok_or_else(|| {
            UnsupportedResolvedNode::new(
                &function.node_id,
                &function.node_id,
                "missing ResolvedCallable",
            )
        })?;
        require_resolved_native_callable(program, callable)?;
    }
    Ok(())
}

pub(super) fn require_resolved_native_callable(
    program: &CheckedProgram,
    callable: &ResolvedCallable,
) -> Result<(), UnsupportedResolvedNode> {
    if !callable.contracts.is_empty() {
        return Err(UnsupportedResolvedNode::new(
            &callable.owner,
            &callable.owner,
            "contracts are not in the resolved native slice",
        ));
    }
    require_scalar_type(program, &callable.owner, &callable.signature.result)?;
    for parameter in &callable.signature.parameters {
        require_scalar_type(program, &callable.owner, &parameter.ty)?;
    }
    require_block(program, &callable.owner, &callable.body.root)
}

fn require_scalar_type(
    program: &CheckedProgram,
    owner: &NodeId,
    ty: &ResolvedTypeId,
) -> Result<(), UnsupportedResolvedNode> {
    match program.resolved_types().get(ty) {
        Some(ResolvedType::Primitive(
            PrimitiveType::I32
            | PrimitiveType::I64
            | PrimitiveType::F64
            | PrimitiveType::Bool
            | PrimitiveType::Unit
            | PrimitiveType::String,
        )) => Ok(()),
        Some(ResolvedType::Tuple(elements)) => {
            for element in elements {
                require_scalar_type(program, owner, element)?;
            }
            Ok(())
        }
        Some(other) => Err(UnsupportedResolvedNode::new(
            owner,
            owner,
            format!("type {other:?} is not in the resolved native slice"),
        )),
        None => Err(UnsupportedResolvedNode::new(
            owner,
            owner,
            format!("missing canonical type '{}'", ty.as_str()),
        )),
    }
}

fn require_block(
    program: &CheckedProgram,
    owner: &NodeId,
    block: &ResolvedBlock,
) -> Result<(), UnsupportedResolvedNode> {
    for statement in &block.statements {
        if !statement.backend_requirements.is_empty() {
            return Err(UnsupportedResolvedNode::new(
                owner,
                &statement.node_id,
                "unmet body backend requirement",
            ));
        }
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(initializer),
            } => {
                require_binding_pattern(owner, pattern)?;
                require_expr(program, owner, initializer)?;
            }
            ResolvedStmtKind::Assign {
                target,
                value,
                conversion,
            } => {
                require_root_place(owner, &statement.node_id, target)?;
                require_conversion(owner, &statement.node_id, conversion.kind)?;
                require_expr(program, owner, value)?;
            }
            ResolvedStmtKind::Return { value, conversion } => {
                if let Some(value) = value {
                    require_expr(program, owner, value)?;
                }
                if let Some(conversion) = conversion {
                    require_conversion(owner, &statement.node_id, conversion.kind)?;
                }
            }
            ResolvedStmtKind::Expr(expression) => require_expr(program, owner, expression)?,
            ResolvedStmtKind::Bind {
                pattern,
                initializer: None,
            } => {
                require_binding_pattern(owner, pattern)?;
            }
            ResolvedStmtKind::While { condition, body } => {
                require_condition(program, owner, condition)?;
                require_block(program, owner, body)?;
            }
            ResolvedStmtKind::For {
                pattern,
                iterable,
                body,
            } => {
                require_binding_pattern(owner, pattern)?;
                match &iterable.kind {
                    ResolvedExprKind::Range { start, end } => {
                        require_integer_expr(program, owner, start)?;
                        require_integer_expr(program, owner, end)?;
                    }
                    _ => {
                        return Err(UnsupportedResolvedNode::new(
                            owner,
                            &statement.node_id,
                            "only range iterables are in the resolved native slice",
                        ))
                    }
                }
                require_block(program, owner, body)?;
            }
            ResolvedStmtKind::Break(value) => {
                if value.is_some() {
                    return Err(UnsupportedResolvedNode::new(
                        owner,
                        &statement.node_id,
                        "break with a value is not in the resolved native slice",
                    ));
                }
            }
            ResolvedStmtKind::Continue => {}
            ResolvedStmtKind::Scope { kind, body } => {
                // Only plain lexical scopes are in the resolved native slice.
                if !matches!(kind, crate::core::ir::ResolvedScopeKind::Lexical) {
                    return Err(UnsupportedResolvedNode::new(
                        owner,
                        &statement.node_id,
                        format!("scope kind {kind:?} is not in the resolved native slice"),
                    ));
                }
                require_block(program, owner, body)?;
            }
            ResolvedStmtKind::Loop(body) => {
                require_block(program, owner, body)?;
            }
            // Specification-level statements: no codegen output, accept unconditionally.
            ResolvedStmtKind::Drop(_) => {}
            ResolvedStmtKind::Contract { condition, .. } => {
                require_expr(program, owner, condition)?;
            }
            ResolvedStmtKind::Math(conditions) => {
                for condition in conditions {
                    require_expr(program, owner, condition)?;
                }
            }
            other => {
                return Err(UnsupportedResolvedNode::new(
                    owner,
                    &statement.node_id,
                    format!("statement {other:?} is not in the resolved native slice"),
                ))
            }
        }
    }
    if let Some(result) = &block.result {
        require_expr(program, owner, result)?;
    }
    Ok(())
}

fn require_binding_pattern(
    owner: &NodeId,
    pattern: &ResolvedPattern,
) -> Result<(), UnsupportedResolvedNode> {
    match pattern.kind {
        ResolvedPatternKind::Binding {
            by_reference: None, ..
        }
        | ResolvedPatternKind::Wildcard => Ok(()),
        _ => Err(UnsupportedResolvedNode::new(
            owner,
            &pattern.node_id,
            "only value bindings and wildcards are in the resolved native slice",
        )),
    }
}

fn require_root_place(
    owner: &NodeId,
    node: &NodeId,
    place: &ResolvedPlace,
) -> Result<(), UnsupportedResolvedNode> {
    for projection in &place.projections {
        match projection {
            crate::core::ir::ResolvedProjection::Tuple { .. } => {}
            other => {
                return Err(UnsupportedResolvedNode::new(
                    owner,
                    node,
                    format!("projection {other:?} is not in the resolved native slice"),
                ))
            }
        }
    }
    Ok(())
}

fn require_conversion(
    owner: &NodeId,
    node: &NodeId,
    conversion: CheckedConversionKind,
) -> Result<(), UnsupportedResolvedNode> {
    if matches!(
        conversion,
        CheckedConversionKind::Identity
            | CheckedConversionKind::NumericWiden
            | CheckedConversionKind::NumericNarrowChecked
    ) {
        Ok(())
    } else {
        Err(UnsupportedResolvedNode::new(
            owner,
            node,
            format!("conversion {conversion:?} is not in the resolved native slice"),
        ))
    }
}

fn require_expr(
    program: &CheckedProgram,
    owner: &NodeId,
    expression: &ResolvedExpr,
) -> Result<(), UnsupportedResolvedNode> {
    if !expression.backend_requirements.is_empty() {
        return Err(UnsupportedResolvedNode::new(
            owner,
            &expression.node_id,
            "unmet expression backend requirement",
        ));
    }
    require_scalar_type(program, &expression.node_id, &expression.ty)?;
    match &expression.kind {
        ResolvedExprKind::Literal(_) => Ok(()),
        ResolvedExprKind::Constant(_) => Ok(()),
        ResolvedExprKind::Load(place) => require_root_place(owner, &expression.node_id, place),
        ResolvedExprKind::Tuple(elements) => {
            for element in elements {
                require_expr(program, owner, element)?;
            }
            Ok(())
        }
        ResolvedExprKind::Project { value, projection } => {
            match projection {
                crate::core::ir::ResolvedValueProjection::Tuple(_) => {}
                other => {
                    return Err(UnsupportedResolvedNode::new(
                        owner,
                        &expression.node_id,
                        format!("value projection {other:?} is not in the resolved native slice"),
                    ))
                }
            }
            require_expr(program, owner, value)
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            require_expr(program, owner, left)?;
            require_expr(program, owner, right)
        }
        ResolvedExprKind::Unary { op, operand }
            if matches!(
                op,
                crate::core::ir::ResolvedUnaryOp::Negate | crate::core::ir::ResolvedUnaryOp::Not
            ) =>
        {
            require_expr(program, owner, operand)
        }
        ResolvedExprKind::Cast { value, conversion } => {
            require_conversion(owner, &expression.node_id, conversion.kind)?;
            require_expr(program, owner, value)
        }
        ResolvedExprKind::Call(call)
            if matches!(
                call.callee,
                ResolvedCallee::Function(_) | ResolvedCallee::Builtin(_)
            ) =>
        {
            for argument in &call.arguments {
                require_conversion(owner, &argument.value.node_id, argument.conversion.kind)?;
                require_expr(program, owner, &argument.value)?;
            }
            Ok(())
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            require_condition(program, owner, condition)?;
            require_block(program, owner, then_block)?;
            require_block(program, owner, else_block)
        }
        ResolvedExprKind::Block(block) => require_block(program, owner, block),
        ResolvedExprKind::Scope { kind, body } => {
            if !matches!(kind, crate::core::ir::ResolvedScopeKind::Lexical) {
                return Err(UnsupportedResolvedNode::new(
                    owner,
                    &expression.node_id,
                    format!("scope kind {kind:?} is not in the resolved native slice"),
                ));
            }
            require_block(program, owner, body)
        }
        other => Err(UnsupportedResolvedNode::new(
            owner,
            &expression.node_id,
            format!("expression {other:?} is not in the resolved native slice"),
        )),
    }
}

/// Condition expressions must be canonical `bool`.
fn require_condition(
    program: &CheckedProgram,
    owner: &NodeId,
    condition: &ResolvedExpr,
) -> Result<(), UnsupportedResolvedNode> {
    require_expr(program, owner, condition)?;
    match program.resolved_types().get(&condition.ty) {
        Some(ResolvedType::Primitive(PrimitiveType::Bool)) => Ok(()),
        Some(other) => Err(UnsupportedResolvedNode::new(
            owner,
            &condition.node_id,
            format!("condition type {other:?} is not bool"),
        )),
        None => Err(UnsupportedResolvedNode::new(
            owner,
            &condition.node_id,
            "condition has a missing canonical type",
        )),
    }
}

/// Range bounds must be signed or unsigned integers (not float/bool).
fn require_integer_expr(
    program: &CheckedProgram,
    owner: &NodeId,
    expression: &ResolvedExpr,
) -> Result<(), UnsupportedResolvedNode> {
    require_expr(program, owner, expression)?;
    match program.resolved_types().get(&expression.ty) {
        Some(ResolvedType::Primitive(
            PrimitiveType::I8
            | PrimitiveType::I16
            | PrimitiveType::I32
            | PrimitiveType::I64
            | PrimitiveType::I128
            | PrimitiveType::Isize
            | PrimitiveType::U8
            | PrimitiveType::U16
            | PrimitiveType::U32
            | PrimitiveType::U64
            | PrimitiveType::U128
            | PrimitiveType::Usize,
        )) => Ok(()),
        Some(other) => Err(UnsupportedResolvedNode::new(
            owner,
            &expression.node_id,
            format!("range bound type {other:?} is not an integer"),
        )),
        None => Err(UnsupportedResolvedNode::new(
            owner,
            &expression.node_id,
            "range bound has a missing canonical type",
        )),
    }
}
