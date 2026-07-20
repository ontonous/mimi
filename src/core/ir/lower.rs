//! Surface-independent construction of typed callable bodies.
//!
//! This module is deliberately separate from `resolved`: the latter owns the
//! stable identity catalog, while this lowering consumes those identities and
//! checker-finalized types.  It must never infer a type or resolve a name.

use super::{
    CheckedConversion, CheckedConversionKind, EffectId, MatchArm as ResolvedMatchArm,
    ResolvedArgument, ResolvedBinaryOp, ResolvedBlock, ResolvedBody, ResolvedBodyError,
    ResolvedCall, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedLambda, ResolvedLiteral,
    ResolvedLocal, ResolvedLocalId, ResolvedPattern, ResolvedPatternKind, ResolvedPlace,
    ResolvedProjection, ResolvedRecordField, ResolvedSignature, ResolvedStmt, ResolvedStmtKind,
    ResolvedType, ResolvedTypeId, ResolvedTypeTable, ResolvedUnaryOp, ResolvedValueProjection,
};
use crate::ast::{
    AstOrigin, BinOp, Expr, File, FuncDef, Item, Lit, Param, Pattern, PatternKind, Stmt, UnOp,
};
use crate::core::resolved::{
    expr_kind, expr_sibling_role, impl_method_owner, interpolation_role, map_entry_role,
    match_arm_role, nested_function_owner, pattern_kind, pattern_sibling_role, stable_id_fragment,
    stmt_anchor, stmt_kind, stmt_sibling_role, type_kind, NodeIdBuilder,
};
use crate::core::{
    CheckedProgram, NodeId, NodeMeta, Origin, ResolvedActor, ResolvedCallKind, ResolvedCallSite,
    ResolvedConstant, ResolvedExternBlock, ResolvedFunction, ResolvedImpl, ResolvedTrait,
    ResolvedTypeDef, ResolvedTypeKind, ResolvedVariantSchema, ResolvedVariantShape,
};
use crate::diagnostic::Diagnostic;
use crate::span::{SourceRegistry, Span};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const BLOCK_NORMALIZATION_RULE: &str = "resolved_body.structured_block";

fn is_local_place(expression: &Expr) -> bool {
    match expression.unlocated() {
        Expr::Ident(_) => true,
        Expr::Field(base, _)
        | Expr::TupleIndex(base, _)
        | Expr::Index(base, _)
        | Expr::Unary(UnOp::Deref, base) => is_local_place(base),
        _ => false,
    }
}

/// Inputs already finalized by the checker and stable resolved walker.
pub struct FunctionBodyInput<'a> {
    pub function: &'a FuncDef,
    pub signature: &'a ResolvedSignature,
    pub signatures: &'a BTreeMap<NodeId, ResolvedSignature>,
    pub functions: &'a HashMap<NodeId, ResolvedFunction>,
    pub type_defs: &'a HashMap<NodeId, ResolvedTypeDef>,
    pub variants: &'a BTreeMap<NodeId, ResolvedVariantSchema>,
    pub actors: &'a HashMap<NodeId, ResolvedActor>,
    pub flows: &'a HashMap<crate::core::FlowId, crate::core::ResolvedFlow>,
    pub traits: &'a HashMap<NodeId, ResolvedTrait>,
    pub impls: &'a HashMap<NodeId, ResolvedImpl>,
    pub field_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    pub type_targets: &'a BTreeMap<NodeId, ResolvedTypeId>,
    pub call_sites: &'a HashMap<NodeId, ResolvedCallSite>,
    pub extern_blocks: &'a HashMap<NodeId, ResolvedExternBlock>,
    pub constants: &'a HashMap<NodeId, ResolvedConstant>,
    pub node_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    pub type_operands: &'a BTreeMap<NodeId, ResolvedTypeId>,
    pub type_arguments: &'a BTreeMap<NodeId, Vec<ResolvedTypeId>>,
    pub types: &'a ResolvedTypeTable,
    pub node_meta: &'a HashMap<NodeId, NodeMeta>,
    pub sources: &'a SourceRegistry,
}

/// Lower the structural core of a checked function without re-running name or
/// type resolution. Unsupported constructs are errors, never omitted nodes.
pub fn lower_function_body(
    input: FunctionBodyInput<'_>,
) -> Result<ResolvedBody, Vec<ResolvedBodyError>> {
    lower_function_body_with_captures(input, BTreeMap::new()).map(|lowered| lowered.body)
}

struct LoweredFunctionBody {
    body: ResolvedBody,
    nested_environments: BTreeMap<NodeId, BTreeMap<String, ResolvedLocalId>>,
}

fn lower_function_body_with_captures(
    input: FunctionBodyInput<'_>,
    captures: BTreeMap<String, ResolvedLocal>,
) -> Result<LoweredFunctionBody, Vec<ResolvedBodyError>> {
    let unit = input
        .types
        .iter()
        .find_map(|(id, ty)| {
            matches!(ty, ResolvedType::Primitive(super::PrimitiveType::Unit)).then(|| id.clone())
        })
        .ok_or_else(|| {
            vec![ResolvedBodyError::new(
                input.signature.owner.clone(),
                "canonical type table has no unit type",
            )]
        })?;
    let capture_ids = captures
        .values()
        .map(|local| local.id.clone())
        .collect::<BTreeSet<_>>();
    let capture_scope = captures
        .iter()
        .map(|(name, local)| (name.clone(), local.id.clone()))
        .collect();
    let capture_locals = captures
        .into_values()
        .map(|local| (local.id.clone(), local))
        .collect();
    let mut lowerer = BodyLowerer {
        owner: input.signature.owner.clone(),
        fallback: input.function.meta.span,
        function: input.function,
        signature: input.signature,
        signatures: input.signatures,
        functions: input.functions,
        type_defs: input.type_defs,
        variants: input.variants,
        actors: input.actors,
        flows: input.flows,
        traits: input.traits,
        impls: input.impls,
        field_types: input.field_types,
        type_targets: input.type_targets,
        call_sites: input.call_sites,
        extern_blocks: input.extern_blocks,
        constants: input.constants,
        node_types: input.node_types,
        type_operands: input.type_operands,
        type_arguments: input.type_arguments,
        types: input.types,
        node_meta: input.node_meta,
        ids: NodeIdBuilder::new(input.sources),
        unit,
        locals: capture_locals,
        parameters: Vec::new(),
        place_inputs: BTreeMap::new(),
        default_values: BTreeMap::new(),
        capture_candidates: capture_ids,
        callable_captures: BTreeSet::new(),
        lambda_contexts: Vec::new(),
        scopes: vec![capture_scope],
        nested_environments: BTreeMap::new(),
    };
    lowerer.install_parameters()?;
    lowerer.lower_default_values()?;
    let root = lowerer.lower_block(
        &input.function.body,
        "body",
        input.signature.result.clone(),
        true,
    )?;
    let body = ResolvedBody {
        owner: input.signature.owner.clone(),
        locals: lowerer.locals,
        parameters: lowerer.parameters,
        captures: lowerer.callable_captures.iter().cloned().collect(),
        place_inputs: lowerer.place_inputs,
        default_values: lowerer.default_values,
        root,
    };
    body.validate(input.types)?;
    Ok(LoweredFunctionBody {
        body,
        nested_environments: lowerer.nested_environments,
    })
}

/// Lower every function currently represented by the canonical signature
/// catalog. The operation is transactional: no partial body map is returned.
pub fn lower_checked_function_bodies(
    file: &File,
    program: &CheckedProgram,
) -> Result<BTreeMap<NodeId, ResolvedBody>, Vec<ResolvedBodyError>> {
    let mut syntax = BTreeMap::new();
    collect_function_syntax(&file.items, "", &mut syntax);
    let mut bodies = BTreeMap::new();
    let mut environments = BTreeMap::<NodeId, BTreeMap<String, ResolvedLocal>>::new();
    let mut errors = Vec::new();
    let mut owners = program.functions().keys().collect::<Vec<_>>();
    owners.sort();
    for owner in owners {
        let Some(signature) = program.resolved_signature(owner) else {
            errors.push(ResolvedBodyError::new(
                owner.clone(),
                "resolved function has no canonical signature",
            ));
            continue;
        };
        let Some(function) = syntax.get(owner) else {
            errors.push(ResolvedBodyError::new(
                owner.clone(),
                "canonical signature has no normalized function body",
            ));
            continue;
        };
        let captures = environments.remove(owner).unwrap_or_default();
        match lower_function_body_with_captures(
            FunctionBodyInput {
                function,
                signature,
                signatures: program.resolved_signatures(),
                functions: program.functions(),
                type_defs: program.type_defs(),
                variants: program.resolved_variants(),
                actors: program.actors(),
                flows: program.flows(),
                traits: program.traits(),
                impls: program.impls(),
                field_types: program.resolved_field_types(),
                type_targets: program.resolved_type_targets(),
                call_sites: program.call_sites(),
                extern_blocks: program.extern_blocks(),
                constants: program.constants(),
                node_types: program.resolved_node_types(),
                type_operands: program.resolved_type_operands(),
                type_arguments: program.resolved_type_arguments(),
                types: program.resolved_types(),
                node_meta: program.node_meta(),
                sources: &file.sources,
            },
            captures,
        ) {
            Ok(lowered) => {
                for (nested, environment) in &lowered.nested_environments {
                    let mut captures = BTreeMap::new();
                    for (name, local) in environment {
                        let Some(local) = lowered.body.locals.get(local) else {
                            errors.push(ResolvedBodyError::new(
                                nested.clone(),
                                format!("captured local '{name}' is absent from enclosing body"),
                            ));
                            continue;
                        };
                        captures.insert(name.clone(), local.clone());
                    }
                    if environments.insert(nested.clone(), captures).is_some() {
                        errors.push(ResolvedBodyError::new(
                            nested.clone(),
                            "nested callable received more than one lexical environment",
                        ));
                    }
                }
                bodies.insert(owner.clone(), lowered.body);
            }
            Err(mut body_errors) => errors.append(&mut body_errors),
        }
    }
    if errors.is_empty() {
        Ok(bodies)
    } else {
        Err(errors)
    }
}

/// Lower every user-implemented Flow transition represented by the canonical
/// signature catalog. Bodyless and runtime-generated matrix transitions remain
/// declaration-only until their generated expression roles are type-keyed.
pub fn lower_checked_transition_bodies(
    file: &File,
    program: &CheckedProgram,
) -> Result<BTreeMap<NodeId, ResolvedBody>, Vec<ResolvedBodyError>> {
    let mut syntax = BTreeMap::new();
    collect_transition_syntax(&file.items, "", &mut syntax);
    let mut bodies = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, transition) in syntax {
        let Some(body) = &transition.body else {
            continue;
        };
        let Some(signature) = program.resolved_signature(&owner) else {
            errors.push(ResolvedBodyError::new(
                owner.clone(),
                "implemented transition has no canonical signature",
            ));
            continue;
        };
        let function = FuncDef {
            meta: transition.meta,
            name: transition.name.clone(),
            pub_: false,
            params: transition.params.clone(),
            ret: None,
            body: body.clone(),
            where_clause: Vec::new(),
            generics: Vec::new(),
            effects: Vec::new(),
            is_comptime: false,
            is_async: false,
            extern_abi: None,
        };
        match lower_function_body(FunctionBodyInput {
            function: &function,
            signature,
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        }) {
            Ok(body) => {
                bodies.insert(owner, body);
            }
            Err(mut body_errors) => errors.append(&mut body_errors),
        }
    }
    if errors.is_empty() {
        Ok(bodies)
    } else {
        Err(errors)
    }
}

/// Transactionally lower every function/method and user-implemented
/// transition currently admitted to the owned typed-body boundary.
pub fn lower_checked_callable_bodies(
    file: &File,
    program: &CheckedProgram,
) -> Result<BTreeMap<NodeId, ResolvedBody>, Vec<ResolvedBodyError>> {
    let functions = lower_checked_function_bodies(file, program)?;
    let transitions = lower_checked_transition_bodies(file, program)?;
    let mut bodies = functions;
    let mut errors = Vec::new();
    for (owner, body) in transitions {
        if bodies.insert(owner.clone(), body).is_some() {
            errors.push(ResolvedBodyError::new(
                owner,
                "callable body identity is shared by a function and transition",
            ));
        }
    }
    if errors.is_empty() {
        Ok(bodies)
    } else {
        Err(errors)
    }
}

fn collect_function_syntax<'a>(
    items: &'a [Item],
    module: &str,
    out: &mut BTreeMap<NodeId, &'a FuncDef>,
) {
    for item in items {
        match item {
            Item::Module(module_def) => {
                let qualified = if module.is_empty() {
                    module_def.name.clone()
                } else {
                    format!("{module}::{}", module_def.name)
                };
                collect_function_syntax(&module_def.items, &qualified, out);
            }
            Item::Func(function) => {
                let qualified = if module.is_empty() {
                    function.name.clone()
                } else {
                    format!("{module}::{}", function.name)
                };
                let owner = NodeId(format!("function:{qualified}"));
                out.insert(owner.clone(), function);
                collect_nested_function_syntax(&function.body, &owner, out);
            }
            Item::Actor(actor) => {
                let qualified = if module.is_empty() {
                    actor.name.clone()
                } else {
                    format!("{module}::{}", actor.name)
                };
                for method in &actor.methods {
                    let owner = NodeId(format!("function:{qualified}::{}", method.name));
                    out.insert(owner.clone(), method);
                    collect_nested_function_syntax(&method.body, &owner, out);
                }
            }
            Item::Impl(impl_def) => {
                let qualified = if module.is_empty() {
                    format!("{}:for:{}", impl_def.trait_name, impl_def.type_name)
                } else {
                    format!(
                        "{module}::{}:for:{}",
                        impl_def.trait_name, impl_def.type_name
                    )
                };
                for method in &impl_def.methods {
                    let owner = impl_method_owner(&qualified, method);
                    out.insert(owner.clone(), method);
                    collect_nested_function_syntax(&method.body, &owner, out);
                }
            }
            _ => {}
        }
    }
}

fn collect_nested_function_syntax<'a>(
    block: &'a [Stmt],
    owner: &NodeId,
    out: &mut BTreeMap<NodeId, &'a FuncDef>,
) {
    for statement in block {
        match statement.unlocated() {
            Stmt::Func(function) => {
                let nested = nested_function_owner(owner, function);
                out.insert(nested.clone(), function);
                collect_nested_function_syntax(&function.body, &nested, out);
            }
            Stmt::If { then_, else_, .. } => {
                collect_nested_function_syntax(then_, owner, out);
                if let Some(else_) = else_ {
                    collect_nested_function_syntax(else_, owner, out);
                }
            }
            Stmt::While { body, .. }
            | Stmt::WhileLet { body, .. }
            | Stmt::Loop(body)
            | Stmt::For { body, .. }
            | Stmt::Block(body)
            | Stmt::Arena(body)
            | Stmt::Unsafe(body)
            | Stmt::OnFailure(body)
            | Stmt::Do(body)
            | Stmt::Parasteps(body)
            | Stmt::Alloc { body, .. }
            | Stmt::Pinned { body, .. } => collect_nested_function_syntax(body, owner, out),
            _ => {}
        }
    }
}

fn collect_transition_syntax<'a>(
    items: &'a [Item],
    module: &str,
    out: &mut BTreeMap<NodeId, &'a crate::ast::TransitionDef>,
) {
    for item in items {
        match item {
            Item::Module(module_def) => {
                let qualified = if module.is_empty() {
                    module_def.name.clone()
                } else {
                    format!("{module}::{}", module_def.name)
                };
                collect_transition_syntax(&module_def.items, &qualified, out);
            }
            Item::Flow(flow) => {
                let qualified = if module.is_empty() {
                    flow.name.clone()
                } else {
                    format!("{module}::{}", flow.name)
                };
                for transition in &flow.transitions {
                    if !matches!(transition.meta.origin, AstOrigin::User) {
                        continue;
                    }
                    out.insert(
                        NodeId(format!(
                            "transition:{qualified}::{}::{}",
                            transition.name, transition.from_state
                        )),
                        transition,
                    );
                }
            }
            _ => {}
        }
    }
}

struct IfControlInput<'a> {
    statement: &'a NodeId,
    origin: &'a Origin,
    condition: &'a Expr,
    then_block: &'a [Stmt],
    else_block: Option<&'a [Stmt]>,
    role: &'a str,
    result_type: ResolvedTypeId,
    has_tail_result: bool,
}

type InstantiatedVariantField = (String, NodeId, ResolvedTypeId);
type InstantiatedVariant = (NodeId, Vec<InstantiatedVariantField>);

struct BodyLowerer<'a> {
    owner: NodeId,
    fallback: Span,
    function: &'a FuncDef,
    signature: &'a ResolvedSignature,
    signatures: &'a BTreeMap<NodeId, ResolvedSignature>,
    functions: &'a HashMap<NodeId, ResolvedFunction>,
    type_defs: &'a HashMap<NodeId, ResolvedTypeDef>,
    variants: &'a BTreeMap<NodeId, ResolvedVariantSchema>,
    actors: &'a HashMap<NodeId, ResolvedActor>,
    flows: &'a HashMap<crate::core::FlowId, crate::core::ResolvedFlow>,
    traits: &'a HashMap<NodeId, ResolvedTrait>,
    impls: &'a HashMap<NodeId, ResolvedImpl>,
    field_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    type_targets: &'a BTreeMap<NodeId, ResolvedTypeId>,
    call_sites: &'a HashMap<NodeId, ResolvedCallSite>,
    extern_blocks: &'a HashMap<NodeId, ResolvedExternBlock>,
    constants: &'a HashMap<NodeId, ResolvedConstant>,
    node_types: &'a BTreeMap<NodeId, ResolvedTypeId>,
    type_operands: &'a BTreeMap<NodeId, ResolvedTypeId>,
    type_arguments: &'a BTreeMap<NodeId, Vec<ResolvedTypeId>>,
    types: &'a ResolvedTypeTable,
    node_meta: &'a HashMap<NodeId, NodeMeta>,
    ids: NodeIdBuilder<'a>,
    unit: ResolvedTypeId,
    locals: BTreeMap<ResolvedLocalId, ResolvedLocal>,
    parameters: Vec<ResolvedLocalId>,
    place_inputs: BTreeMap<NodeId, ResolvedExpr>,
    default_values: BTreeMap<super::ResolvedParameterId, ResolvedExpr>,
    capture_candidates: BTreeSet<ResolvedLocalId>,
    callable_captures: BTreeSet<ResolvedLocalId>,
    lambda_contexts: Vec<LambdaCaptureContext>,
    scopes: Vec<BTreeMap<String, ResolvedLocalId>>,
    nested_environments: BTreeMap<NodeId, BTreeMap<String, ResolvedLocalId>>,
}

struct LambdaCaptureContext {
    owned: BTreeSet<ResolvedLocalId>,
    captures: BTreeSet<ResolvedLocalId>,
}

impl BodyLowerer<'_> {
    fn install_parameters(&mut self) -> Result<(), Vec<ResolvedBodyError>> {
        if self.signature.parameters.is_empty() {
            return Ok(());
        }
        for parameter in &self.signature.parameters {
            let origin = self.origin(&parameter.id.0)?;
            let local_id = ResolvedLocalId(NodeId(format!("{}/local", parameter.id.0 .0)));
            self.parameters.push(local_id.clone());
            self.insert_local(
                parameter.name.clone(),
                ResolvedLocal {
                    id: local_id,
                    display_name: parameter.name.clone(),
                    ty: parameter.ty.clone(),
                    mutable: parameter.mutable,
                    origin,
                },
                &parameter.id.0,
            )?;
        }
        Ok(())
    }

    fn lower_default_values(&mut self) -> Result<(), Vec<ResolvedBodyError>> {
        let parameters = self.function.params.clone();
        for parameter in &parameters {
            let Some(default) = &parameter.default_value else {
                continue;
            };
            let resolved = self
                .signature
                .parameters
                .iter()
                .find(|candidate| candidate.name == parameter.name)
                .ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        self.owner.clone(),
                        format!(
                            "default parameter '{}' has no canonical signature identity",
                            parameter.name
                        ),
                    )]
                })?;
            let value = self.lower_expr(
                default,
                &format!("parameter.{}.default", stable_id_fragment(&parameter.name)),
            )?;
            let value = self.apply_implicit_conversion(&resolved.id.0, value, &resolved.ty)?;
            if self
                .default_values
                .insert(resolved.id.clone(), value)
                .is_some()
            {
                return Err(vec![ResolvedBodyError::new(
                    resolved.id.0.clone(),
                    "parameter default identity collision",
                )]);
            }
        }
        Ok(())
    }

    fn lower_block(
        &mut self,
        block: &[Stmt],
        role: &str,
        block_ty: ResolvedTypeId,
        has_tail_result: bool,
    ) -> Result<ResolvedBlock, Vec<ResolvedBodyError>> {
        let (node_id, origin) = self.block_identity(role, block);
        self.scopes.push(BTreeMap::new());
        let tail_index = has_tail_result
            .then(|| block.len().checked_sub(1))
            .flatten()
            .filter(|index| {
                matches!(
                    block[*index].unlocated(),
                    Stmt::Expr(_) | Stmt::If { else_: Some(_), .. }
                )
            });
        let mut statements = Vec::with_capacity(block.len());
        let mut result = None;
        for index in 0..block.len() {
            let stmt_role = stmt_sibling_role(role, block, index);
            if Some(index) == tail_index {
                let lowered = match block[index].unlocated() {
                    Stmt::Expr(expression) => {
                        self.lower_expr(expression, &format!("{stmt_role}.expression"))?
                    }
                    Stmt::If { cond, then_, else_ } => {
                        let statement_id = self.stmt_id(&block[index], &stmt_role)?;
                        let statement_origin = self.origin(&statement_id)?;
                        self.lower_if_control(IfControlInput {
                            statement: &statement_id,
                            origin: &statement_origin,
                            condition: cond,
                            then_block: then_,
                            else_block: else_.as_deref(),
                            role: &stmt_role,
                            result_type: block_ty.clone(),
                            has_tail_result: true,
                        })?
                    }
                    _ => {
                        self.scopes.pop();
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "tail selection is not a value-producing statement",
                        )]);
                    }
                };
                let lowered = self.apply_implicit_conversion(&node_id, lowered, &block_ty)?;
                result = Some(Box::new(lowered));
            } else if let Some(statement) = self.lower_stmt(&block[index], &stmt_role)? {
                statements.push(statement);
            }
        }
        self.scopes.pop();
        Ok(ResolvedBlock {
            node_id,
            origin,
            ty: block_ty,
            statements,
            result,
        })
    }

    fn lower_if_control(
        &mut self,
        input: IfControlInput<'_>,
    ) -> Result<ResolvedExpr, Vec<ResolvedBodyError>> {
        let condition = self.lower_expr(input.condition, &format!("{}.condition", input.role))?;
        let then_block = self.lower_block(
            input.then_block,
            &format!("{}.then", input.role),
            input.result_type.clone(),
            input.has_tail_result,
        )?;
        let else_block = self.lower_block(
            input.else_block.unwrap_or_default(),
            &format!("{}.else", input.role),
            input.result_type.clone(),
            input.has_tail_result,
        )?;
        Ok(ResolvedExpr {
            node_id: NodeId(format!("{}/control-expression", input.statement.0)),
            origin: input.origin.clone(),
            ty: input.result_type,
            effects: Vec::new(),
            backend_requirements: Vec::new(),
            kind: ResolvedExprKind::If {
                condition: Box::new(condition),
                then_block: Box::new(then_block),
                else_block: Box::new(else_block),
            },
        })
    }

    fn lower_stmt(
        &mut self,
        stmt: &Stmt,
        role: &str,
    ) -> Result<Option<ResolvedStmt>, Vec<ResolvedBodyError>> {
        let node_id = self.stmt_id(stmt, role)?;
        let origin = self.origin(&node_id)?;
        let kind = match stmt.unlocated() {
            Stmt::Let {
                pat,
                ty: declared_type,
                init,
                mut_,
                ref_,
                ..
            } => {
                let mut initializer = init
                    .as_ref()
                    .map(|expr| self.lower_expr(expr, &format!("{role}.initializer")))
                    .transpose()?
                    .ok_or_else(|| {
                        vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "binding without an initializer has no checker-persisted value type",
                        )]
                    })?;
                let mut binding_type = declared_type
                    .as_ref()
                    .filter(|ty| !matches!(ty.unlocated(), crate::ast::Type::Infer))
                    .map(|ty| self.annotation_type(ty, &format!("{role}.type")))
                    .transpose()?
                    .unwrap_or_else(|| initializer.ty.clone());
                if *ref_ {
                    binding_type = self.reference_binding_type(&node_id, &initializer.ty)?;
                    initializer = ResolvedExpr {
                        node_id: NodeId(format!("{}/temporary-borrow", node_id.0)),
                        origin: Origin::Desugared {
                            parent: node_id.clone(),
                            rule: "resolved_body.reference_binding".into(),
                            span: origin.user_span(),
                        },
                        ty: binding_type.clone(),
                        effects: initializer.effects.clone(),
                        backend_requirements: initializer.backend_requirements.clone(),
                        kind: ResolvedExprKind::Unary {
                            op: ResolvedUnaryOp::BorrowShared,
                            operand: Box::new(initializer),
                        },
                    };
                } else {
                    initializer =
                        self.apply_implicit_conversion(&node_id, initializer, &binding_type)?;
                }
                let mut pattern = self.lower_binding_pattern(
                    pat,
                    &format!("{role}.pattern"),
                    binding_type,
                    *mut_,
                )?;
                if *ref_ {
                    let ResolvedPatternKind::Binding { by_reference, .. } = &mut pattern.kind
                    else {
                        return self.unsupported(
                            &node_id,
                            "reference binding with a non-variable pattern",
                        );
                    };
                    *by_reference = Some(super::Permission::View);
                }
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                }
            }
            Stmt::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|expr| self.lower_expr(expr, &format!("{role}.value")))
                    .transpose()?;
                let conversion = value
                    .as_ref()
                    .map(|value| {
                        self.implicit_conversion(&node_id, &value.ty, &self.signature.result)
                    })
                    .transpose()?;
                ResolvedStmtKind::Return { value, conversion }
            }
            Stmt::Break(value) => ResolvedStmtKind::Break(
                value
                    .as_ref()
                    .map(|expr| self.lower_expr(expr, &format!("{role}.value")))
                    .transpose()?,
            ),
            Stmt::Continue => ResolvedStmtKind::Continue,
            Stmt::Expr(expr) => {
                ResolvedStmtKind::Expr(self.lower_expr(expr, &format!("{role}.expression"))?)
            }
            Stmt::Assign { target, value } => {
                let place = self.lower_place(target, &format!("{role}.target"))?;
                let value = self.lower_expr(value, &format!("{role}.value"))?;
                let target_ty = self.place_type(&node_id, &place)?;
                let conversion = self.implicit_conversion(&node_id, &value.ty, &target_ty)?;
                ResolvedStmtKind::Assign {
                    target: place,
                    value,
                    conversion,
                }
            }
            Stmt::If { cond, then_, else_ } => {
                ResolvedStmtKind::Expr(self.lower_if_control(IfControlInput {
                    statement: &node_id,
                    origin: &origin,
                    condition: cond,
                    then_block: then_,
                    else_block: else_.as_deref(),
                    role,
                    result_type: self.unit.clone(),
                    has_tail_result: false,
                })?)
            }
            Stmt::While { cond, body } => ResolvedStmtKind::While {
                condition: self.lower_expr(cond, &format!("{role}.condition"))?,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::WhileLet { pat, init, body } => {
                let initializer = self.lower_expr(init, &format!("{role}.initializer"))?;
                self.scopes.push(BTreeMap::new());
                let lowered = (|| {
                    let pattern = self.lower_binding_pattern(
                        pat,
                        &format!("{role}.pattern"),
                        initializer.ty.clone(),
                        false,
                    )?;
                    let body =
                        self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?;
                    Ok::<_, Vec<ResolvedBodyError>>((pattern, body))
                })();
                self.scopes.pop();
                let (pattern, body) = lowered?;
                ResolvedStmtKind::WhileLet {
                    pattern,
                    initializer,
                    body,
                }
            }
            Stmt::Loop(body) => ResolvedStmtKind::Loop(self.lower_block(
                body,
                &format!("{role}.body"),
                self.unit.clone(),
                false,
            )?),
            Stmt::For {
                var,
                iterable,
                body,
            } => {
                let iterable = self.lower_expr(iterable, &format!("{role}.iterable"))?;
                let element_ty = self.iterable_element_type(&node_id, &iterable.ty)?;
                let pattern_id = NodeId(format!("{}/for-pattern", node_id.0));
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", pattern_id.0)));
                let pattern_origin = Origin::Desugared {
                    parent: node_id.clone(),
                    rule: "resolved_body.for_binding".into(),
                    span: origin.user_span(),
                };
                self.scopes.push(BTreeMap::new());
                self.insert_local(
                    var.clone(),
                    ResolvedLocal {
                        id: local_id.clone(),
                        display_name: var.clone(),
                        ty: element_ty.clone(),
                        mutable: false,
                        origin: pattern_origin.clone(),
                    },
                    &pattern_id,
                )?;
                let lowered_body =
                    self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false);
                self.scopes.pop();
                ResolvedStmtKind::For {
                    pattern: ResolvedPattern {
                        node_id: pattern_id,
                        origin: pattern_origin,
                        ty: element_ty,
                        kind: ResolvedPatternKind::Binding {
                            local: local_id,
                            by_reference: None,
                        },
                    },
                    iterable,
                    body: lowered_body?,
                }
            }
            Stmt::Block(body) | Stmt::Do(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Lexical,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Unsafe(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Unsafe,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::OnFailure(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::FailureGuard,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Arena(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Arena,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Parasteps(body) => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Parallel,
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Alloc { kind, body } => ResolvedStmtKind::Scope {
                kind: super::ResolvedScopeKind::Allocator(match kind {
                    crate::ast::AllocKind::System => super::AllocatorKind::System,
                    crate::ast::AllocKind::Arena => super::AllocatorKind::Arena,
                    crate::ast::AllocKind::Bump => super::AllocatorKind::Bump,
                }),
                body: self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?,
            },
            Stmt::Func(function) => {
                let nested = nested_function_owner(&self.owner, function);
                let mut environment = BTreeMap::new();
                for scope in &self.scopes {
                    environment.extend(
                        scope
                            .iter()
                            .map(|(name, local)| (name.clone(), local.clone())),
                    );
                }
                if self
                    .nested_environments
                    .insert(nested.clone(), environment)
                    .is_some()
                {
                    return Err(vec![ResolvedBodyError::new(
                        nested,
                        "nested callable environment identity collision",
                    )]);
                }
                ResolvedStmtKind::NestedCallable(nested)
            }
            Stmt::Requires(expr, _) => ResolvedStmtKind::Contract {
                kind: super::ContractKind::Requires,
                condition: self.lower_expr(expr, &format!("{role}.expression"))?,
            },
            Stmt::Ensures(expr, _) => {
                self.scopes.push(BTreeMap::new());
                let result_id = NodeId(format!("{}/contract-result", self.owner.0));
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", result_id.0)));
                let result_origin = Origin::Desugared {
                    parent: node_id.clone(),
                    rule: "resolved_body.contract_result".into(),
                    span: origin.user_span(),
                };
                let lowered = (|| {
                    if self.locals.contains_key(&local_id) {
                        self.scopes
                            .last_mut()
                            .ok_or_else(|| {
                                vec![ResolvedBodyError::new(
                                    result_id.clone(),
                                    "contract result has no lexical scope",
                                )]
                            })?
                            .insert("result".into(), local_id.clone());
                    } else {
                        self.insert_local(
                            "result".into(),
                            ResolvedLocal {
                                id: local_id.clone(),
                                display_name: "result".into(),
                                ty: self.signature.result.clone(),
                                mutable: false,
                                origin: result_origin,
                            },
                            &result_id,
                        )?;
                    }
                    self.lower_expr(expr, &format!("{role}.expression"))
                })();
                self.scopes.pop();
                ResolvedStmtKind::Contract {
                    kind: super::ContractKind::Ensures,
                    condition: lowered?,
                }
            }
            Stmt::Invariant(expr, _) => ResolvedStmtKind::Contract {
                kind: super::ContractKind::Invariant,
                condition: self.lower_expr(expr, &format!("{role}.expression"))?,
            },
            Stmt::Math(expressions) => ResolvedStmtKind::Math(
                expressions
                    .iter()
                    .enumerate()
                    .map(|(index, expression)| {
                        self.lower_expr(
                            expression,
                            &expr_sibling_role(&format!("{role}.math"), expressions, index),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Stmt::Drop(expr) => {
                ResolvedStmtKind::Drop(self.lower_drop_places(expr, &format!("{role}.expression"))?)
            }
            Stmt::SharedLet {
                kind, name, init, ..
            } => {
                let value = self.lower_expr(init, &format!("{role}.initializer"))?;
                let local_ty = self.shared_binding_type(&node_id, *kind, &value.ty)?;
                let conversion_kind = match kind {
                    crate::ast::SharedKind::Weak | crate::ast::SharedKind::WeakLocal => {
                        CheckedConversionKind::OwnershipDowngrade
                    }
                    crate::ast::SharedKind::Shared | crate::ast::SharedKind::LocalShared => {
                        CheckedConversionKind::OwnershipWrap
                    }
                };
                let pattern_id = NodeId(format!("{}/shared-pattern", node_id.0));
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", pattern_id.0)));
                let pattern_origin = Origin::Desugared {
                    parent: node_id.clone(),
                    rule: "resolved_body.shared_binding".into(),
                    span: origin.user_span(),
                };
                let initializer = ResolvedExpr {
                    node_id: NodeId(format!("{}/ownership-wrap", node_id.0)),
                    origin: pattern_origin.clone(),
                    ty: local_ty.clone(),
                    effects: Vec::new(),
                    backend_requirements: Vec::new(),
                    kind: ResolvedExprKind::Cast {
                        conversion: CheckedConversion {
                            kind: conversion_kind,
                            from: value.ty.clone(),
                            to: local_ty.clone(),
                        },
                        value: Box::new(value),
                    },
                };
                self.insert_local(
                    name.clone(),
                    ResolvedLocal {
                        id: local_id.clone(),
                        display_name: name.clone(),
                        ty: local_ty.clone(),
                        mutable: false,
                        origin: pattern_origin.clone(),
                    },
                    &pattern_id,
                )?;
                ResolvedStmtKind::Bind {
                    pattern: ResolvedPattern {
                        node_id: pattern_id,
                        origin: pattern_origin,
                        ty: local_ty,
                        kind: ResolvedPatternKind::Binding {
                            local: local_id,
                            by_reference: None,
                        },
                    },
                    initializer: Some(initializer),
                }
            }
            Stmt::Delegate { kind, expr, target } => {
                let source = self.lower_place(expr, &format!("{role}.expression"))?;
                let target = if let Some(local) = self.lookup_local(target) {
                    super::DelegateTarget::Local(local)
                } else {
                    let candidates = self
                        .functions
                        .values()
                        .filter(|function| {
                            function.qualified_name == *target
                                || function
                                    .qualified_name
                                    .rsplit_once("::")
                                    .is_some_and(|(_, short)| short == target)
                        })
                        .collect::<Vec<_>>();
                    let [function] = candidates.as_slice() else {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            format!(
                                "delegate target '{target}' does not resolve to exactly one local or callable"
                            ),
                        )]);
                    };
                    super::DelegateTarget::Callable(function.node_id.clone())
                };
                ResolvedStmtKind::Delegate {
                    permission: match kind {
                        crate::ast::DelegateKind::View => super::Permission::View,
                        crate::ast::DelegateKind::Mutate => super::Permission::Mutate,
                        crate::ast::DelegateKind::Consume => super::Permission::Consume,
                    },
                    source,
                    target,
                }
            }
            Stmt::Pinned {
                expr,
                timeout,
                var,
                body,
            } => {
                let value = self.lower_expr(expr, &format!("{role}.expression"))?;
                let timeout = timeout
                    .as_ref()
                    .map(|timeout| self.lower_expr(timeout, &format!("{role}.timeout")))
                    .transpose()?;
                self.scopes.push(BTreeMap::new());
                let binding = var.as_ref().map(|name| {
                    let binding_id = NodeId(format!("{}/pinned-binding", node_id.0));
                    let local_id = ResolvedLocalId(NodeId(format!("{}/local", binding_id.0)));
                    let binding_origin = Origin::Desugared {
                        parent: node_id.clone(),
                        rule: "resolved_body.pinned_binding".into(),
                        span: origin.user_span(),
                    };
                    self.insert_local(
                        name.clone(),
                        ResolvedLocal {
                            id: local_id.clone(),
                            display_name: name.clone(),
                            ty: value.ty.clone(),
                            mutable: false,
                            origin: binding_origin,
                        },
                        &binding_id,
                    )?;
                    Ok::<_, Vec<ResolvedBodyError>>(local_id)
                });
                let lowered = (|| {
                    let binding = binding.transpose()?;
                    let body =
                        self.lower_block(body, &format!("{role}.body"), self.unit.clone(), false)?;
                    Ok::<_, Vec<ResolvedBodyError>>((binding, body))
                })();
                self.scopes.pop();
                let (binding, body) = lowered?;
                ResolvedStmtKind::Pinned {
                    value,
                    timeout,
                    binding,
                    body,
                }
            }
            Stmt::Desc(..) | Stmt::Rule(..) | Stmt::MmsBlock { .. } => return Ok(None),
            Stmt::Ellipsis => return self.unsupported(&node_id, stmt_kind(stmt)),
            Stmt::Located { .. } => unreachable!("Stmt::unlocated returned Located"),
        };
        let backend_requirements = match &kind {
            ResolvedStmtKind::Pinned { .. } => Some(super::BackendRequirement {
                requirement_id: "RESOURCE-LINEAR-001".into(),
                capability: "ffi.pinned".into(),
            }),
            ResolvedStmtKind::Math(_) => Some(super::BackendRequirement {
                requirement_id: "VERIFY-CORE-001".into(),
                capability: "verification.math".into(),
            }),
            _ => None,
        }
        .into_iter()
        .collect();
        Ok(Some(ResolvedStmt {
            node_id,
            origin,
            ty: self.unit.clone(),
            backend_requirements,
            kind,
        }))
    }

    fn lower_expr(
        &mut self,
        expr: &Expr,
        role: &str,
    ) -> Result<ResolvedExpr, Vec<ResolvedBodyError>> {
        let node_id = self.expr_id(expr, role)?;
        let origin = self.origin(&node_id)?;
        let mut ty = self.node_types.get(&node_id).cloned().ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "expression has no checker-finalized canonical type",
            )]
        })?;
        let kind = match expr.unlocated() {
            Expr::Literal(Lit::FString(parts)) => {
                if let Some((_, target)) = self.instantiated_type_target(&node_id, &ty)? {
                    ty = target;
                }
                let mut lowered = Vec::with_capacity(parts.len());
                for (index, part) in parts.iter().enumerate() {
                    lowered.push(match part {
                        crate::ast::FStringPart::Text(text) => {
                            super::ResolvedFStringPart::Text(text.clone())
                        }
                        crate::ast::FStringPart::Interp(expression) => {
                            super::ResolvedFStringPart::Interpolation(self.lower_expr(
                                expression,
                                &interpolation_role(&format!("{role}.interpolation"), parts, index),
                            )?)
                        }
                    });
                }
                ResolvedExprKind::FString(lowered)
            }
            Expr::Literal(literal) => {
                if let Some((_, target)) = self.instantiated_type_target(&node_id, &ty)? {
                    ty = target;
                }
                ResolvedExprKind::Literal(self.lower_literal(&node_id, literal)?)
            }
            Expr::Ident(name) => {
                if let Some(local) = self.lookup_local(name) {
                    ResolvedExprKind::Load(ResolvedPlace::root(local))
                } else if name == "None" {
                    ResolvedExprKind::Constant(NodeId("builtin:value:None".into()))
                } else {
                    let functions = self
                        .functions
                        .values()
                        .filter(|function| {
                            function.qualified_name == *name
                                || function
                                    .qualified_name
                                    .rsplit_once("::")
                                    .is_some_and(|(_, short)| short == name)
                        })
                        .collect::<Vec<_>>();
                    if let [function] = functions.as_slice() {
                        ResolvedExprKind::Callable(ResolvedCallee::Function(
                            function.node_id.clone(),
                        ))
                    } else {
                        let states = self
                            .flows
                            .values()
                            .flat_map(|flow| flow.states.values())
                            .filter(|state| state.id.name == *name)
                            .filter(|state| {
                                matches!(
                                    self.types.get(&ty),
                                    Some(ResolvedType::Nominal { item, .. })
                                        if item.as_str() == state.node_id.0
                                )
                            })
                            .collect::<Vec<_>>();
                        if let [state] = states.as_slice() {
                            ResolvedExprKind::Constant(state.node_id.clone())
                        } else {
                            let unit_variants = match self.types.get(&ty) {
                                Some(ResolvedType::Nominal { item, .. }) => {
                                    let owner = NodeId(item.as_str().to_string());
                                    self.type_defs.get(&owner).and_then(|definition| {
                                        (definition.kind == ResolvedTypeKind::Enum)
                                            .then(|| definition.variant_ids.get(name))
                                            .flatten()
                                            .and_then(|variant| self.variants.get(variant))
                                            .filter(|variant| {
                                                variant.shape == ResolvedVariantShape::Unit
                                            })
                                            .map(|variant| variant.node_id.clone())
                                    })
                                }
                                _ => None,
                            };
                            if let Some(variant_id) = unit_variants {
                                ResolvedExprKind::Constant(variant_id)
                            } else {
                                let candidates = self
                                    .constants
                                    .values()
                                    .filter(|constant| {
                                        constant.qualified_name == *name
                                            || constant
                                                .qualified_name
                                                .rsplit_once("::")
                                                .is_some_and(|(_, short)| short == name)
                                    })
                                    .collect::<Vec<_>>();
                                let [constant] = candidates.as_slice() else {
                                    return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            format!(
                                "identifier '{name}' does not resolve to exactly one local or constant"
                            ),
                        )]);
                                };
                                ResolvedExprKind::Constant(constant.node_id.clone())
                            }
                        }
                    }
                }
            }
            Expr::Binary(op, left, right) => ResolvedExprKind::Binary {
                op: self.lower_binary(&node_id, *op)?,
                left: Box::new(self.lower_expr(left, &format!("{role}.left"))?),
                right: Box::new(self.lower_expr(right, &format!("{role}.right"))?),
            },
            Expr::Unary(UnOp::Deref, operand) if !is_local_place(expr) => {
                ResolvedExprKind::Project {
                    value: Box::new(self.lower_expr(operand, &format!("{role}.inner"))?),
                    projection: ResolvedValueProjection::Dereference,
                }
            }
            Expr::Field(base, name) if !is_local_place(expr) => {
                let value = self.lower_expr(base, &format!("{role}.inner"))?;
                let field = self.resolve_field(&node_id, &value.ty, name)?;
                ResolvedExprKind::Project {
                    value: Box::new(value),
                    projection: ResolvedValueProjection::Field(field),
                }
            }
            Expr::TupleIndex(base, index) if !is_local_place(expr) => ResolvedExprKind::Project {
                value: Box::new(self.lower_expr(base, &format!("{role}.inner"))?),
                projection: ResolvedValueProjection::Tuple(*index),
            },
            Expr::Index(base, index) if !is_local_place(expr) => ResolvedExprKind::Project {
                value: Box::new(self.lower_expr(base, &format!("{role}.left"))?),
                projection: ResolvedValueProjection::Index(Box::new(
                    self.lower_expr(index, &format!("{role}.right"))?,
                )),
            },
            Expr::Unary(UnOp::Deref, _)
            | Expr::Field(_, _)
            | Expr::Index(_, _)
            | Expr::TupleIndex(_, _) => ResolvedExprKind::Load(self.lower_place(expr, role)?),
            Expr::Unary(op, operand) => ResolvedExprKind::Unary {
                op: self.lower_unary(*op),
                operand: Box::new(self.lower_expr(operand, &format!("{role}.inner"))?),
            },
            Expr::Tuple(values) => {
                ResolvedExprKind::Tuple(self.lower_expr_list(values, &format!("{role}.element"))?)
            }
            Expr::List(values) => {
                ResolvedExprKind::List(self.lower_expr_list(values, &format!("{role}.element"))?)
            }
            Expr::SetLiteral(values) => {
                ResolvedExprKind::Set(self.lower_expr_list(values, &format!("{role}.element"))?)
            }
            Expr::Comprehension {
                expr,
                var,
                iter,
                guard,
            } => {
                let iterable = self.lower_expr(iter, &format!("{role}.iterable"))?;
                let element_ty = self.iterable_element_type(&node_id, &iterable.ty)?;
                let pattern_id = NodeId(format!("{}/comprehension-pattern", node_id.0));
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", pattern_id.0)));
                let pattern_origin = Origin::Desugared {
                    parent: node_id.clone(),
                    rule: "resolved_body.comprehension_binding".into(),
                    span: origin.user_span(),
                };
                self.scopes.push(BTreeMap::new());
                let lowered = (|| {
                    self.insert_local(
                        var.clone(),
                        ResolvedLocal {
                            id: local_id.clone(),
                            display_name: var.clone(),
                            ty: element_ty.clone(),
                            mutable: false,
                            origin: pattern_origin.clone(),
                        },
                        &pattern_id,
                    )?;
                    let value = self.lower_expr(expr, &format!("{role}.value"))?;
                    let guard = guard
                        .as_ref()
                        .map(|guard| self.lower_expr(guard, &format!("{role}.guard")))
                        .transpose()?;
                    Ok::<_, Vec<ResolvedBodyError>>((value, guard))
                })();
                self.scopes.pop();
                let (value, guard) = lowered?;
                ResolvedExprKind::Comprehension {
                    pattern: ResolvedPattern {
                        node_id: pattern_id,
                        origin: pattern_origin,
                        ty: element_ty,
                        kind: ResolvedPatternKind::Binding {
                            local: local_id,
                            by_reference: None,
                        },
                    },
                    value: Box::new(value),
                    iterable: Box::new(iterable),
                    guard: guard.map(Box::new),
                }
            }
            Expr::OptionalChain(receiver, name) => {
                let receiver = self.lower_expr(receiver, &format!("{role}.inner"))?;
                let inner = self.optional_success_type(&node_id, &receiver.ty)?;
                let field = self.resolve_field(&node_id, &inner, name)?;
                let field_type = self.field_types.get(&field).cloned().ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        field.clone(),
                        "optional-chain field has no canonical declaration type",
                    )]
                })?;
                let field_type = self.instantiate_member_type(&node_id, &inner, &field_type)?;
                ResolvedExprKind::OptionalChain {
                    receiver: Box::new(receiver),
                    field,
                    field_type,
                }
            }
            Expr::TypeOf(value) => ResolvedExprKind::TypeOf(Box::new(
                self.lower_expr(value, &format!("{role}.inner"))?,
            )),
            Expr::TypeInfo(_) => ResolvedExprKind::TypeValue(
                self.type_operands.get(&node_id).cloned().ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        "type_info has no checker-resolved canonical type operand",
                    )]
                })?,
            ),
            Expr::Old(value) => {
                ResolvedExprKind::Old(Box::new(self.lower_expr(value, &format!("{role}.inner"))?))
            }
            Expr::Block(block) => ResolvedExprKind::Block(Box::new(self.lower_block(
                block,
                &format!("{role}.block"),
                ty.clone(),
                true,
            )?)),
            Expr::Arena(block) => ResolvedExprKind::Scope {
                kind: super::ResolvedScopeKind::Arena,
                body: Box::new(self.lower_block(
                    block,
                    &format!("{role}.block"),
                    ty.clone(),
                    true,
                )?),
            },
            Expr::Comptime(block) => ResolvedExprKind::Comptime(Box::new(self.lower_block(
                block,
                &format!("{role}.block"),
                ty.clone(),
                true,
            )?)),
            Expr::If { cond, then_, else_ } => {
                let else_ = else_.as_ref().ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        "value-producing if expression has no else branch",
                    )]
                })?;
                ResolvedExprKind::If {
                    condition: Box::new(self.lower_expr(cond, &format!("{role}.condition"))?),
                    then_block: Box::new(self.lower_block(
                        then_,
                        &format!("{role}.then"),
                        ty.clone(),
                        true,
                    )?),
                    else_block: Box::new(self.lower_block(
                        else_,
                        &format!("{role}.else"),
                        ty.clone(),
                        true,
                    )?),
                }
            }
            Expr::Range { start, end } => ResolvedExprKind::Range {
                start: Box::new(self.lower_expr(start, &format!("{role}.start"))?),
                end: Box::new(self.lower_expr(end, &format!("{role}.end"))?),
            },
            Expr::SliceExpr { target, start, end } => ResolvedExprKind::Slice {
                target: Box::new(self.lower_expr(target, &format!("{role}.target"))?),
                start: start
                    .as_ref()
                    .map(|value| {
                        self.lower_expr(value, &format!("{role}.start"))
                            .map(Box::new)
                    })
                    .transpose()?,
                end: end
                    .as_ref()
                    .map(|value| self.lower_expr(value, &format!("{role}.end")).map(Box::new))
                    .transpose()?,
            },
            Expr::Spawn(value) => {
                ResolvedExprKind::Spawn(Box::new(self.lower_expr(value, &format!("{role}.inner"))?))
            }
            Expr::Await(value) => {
                ResolvedExprKind::Await(Box::new(self.lower_expr(value, &format!("{role}.inner"))?))
            }
            Expr::Lambda { params, body, .. } => {
                self.lower_lambda(&node_id, params, body, role, &ty)?
            }
            Expr::Call(callee, arguments) => {
                ResolvedExprKind::Call(self.lower_call(&node_id, callee, arguments, role, &[])?)
            }
            Expr::Turbofish(name, _, arguments) => {
                let type_arguments =
                    self.type_arguments.get(&node_id).cloned().ok_or_else(|| {
                        vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "turbofish call has no canonical generic argument list",
                        )]
                    })?;
                ResolvedExprKind::Call(self.lower_call(
                    &node_id,
                    &Expr::Ident(name.clone()),
                    arguments,
                    role,
                    &type_arguments,
                )?)
            }
            Expr::Match(scrutinee, arms) => {
                let scrutinee = self.lower_expr(scrutinee, &format!("{role}.scrutinee"))?;
                let arms = self.lower_match_arms(arms, role, &scrutinee.ty, &ty)?;
                ResolvedExprKind::Match {
                    scrutinee: Box::new(scrutinee),
                    arms,
                }
            }
            Expr::Record { fields, .. } => self.lower_record(&node_id, fields, role, &ty)?,
            Expr::MapLiteral { entries } => {
                let mut lowered = Vec::with_capacity(entries.len());
                for index in 0..entries.len() {
                    let entry_role = map_entry_role(&format!("{role}.entry"), entries, index);
                    lowered.push((
                        self.lower_expr(&entries[index].0, &format!("{entry_role}.key"))?,
                        self.lower_expr(&entries[index].1, &format!("{entry_role}.value"))?,
                    ));
                }
                ResolvedExprKind::Map(lowered)
            }
            Expr::Try(value) => ResolvedExprKind::Try {
                value: Box::new(self.lower_expr(value, &format!("{role}.inner"))?),
                propagation_target: self.owner.clone(),
            },
            Expr::Cast(value, _) => {
                let value = self.lower_expr(value, &format!("{role}.inner"))?;
                let conversion = self.checked_explicit_conversion(&node_id, &value.ty, &ty)?;
                ResolvedExprKind::Cast {
                    value: Box::new(value),
                    conversion,
                }
            }
            Expr::Quote(_) | Expr::QuoteInterpolate(_) | Expr::NamedArg(_, _) => {
                return self.unsupported(&node_id, expr_kind(expr))
            }
            Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
        };
        let effects = match &kind {
            ResolvedExprKind::Call(call) => call.effects.clone(),
            _ => Vec::new(),
        };
        if let ResolvedExprKind::Load(place) = &kind {
            ty = self.place_type(&node_id, place)?.clone();
        }
        if let ResolvedExprKind::Call(call) = &kind {
            if call.result != ty {
                if self.is_system_fault_refinement(&call.result, &ty) {
                    ty = call.result.clone();
                } else {
                    return Err(vec![ResolvedBodyError::new(
                        node_id.clone(),
                        "call result type disagrees with its closed callable signature",
                    )]);
                }
            }
        }
        let backend_requirements = match &kind {
            ResolvedExprKind::TypeOf(_) => Some(super::BackendRequirement {
                requirement_id: "COMPTIME-PURE-001".into(),
                capability: "reflection.type_name".into(),
            }),
            ResolvedExprKind::TypeValue(_) => Some(super::BackendRequirement {
                requirement_id: "COMPTIME-PURE-001".into(),
                capability: "reflection.type_info".into(),
            }),
            ResolvedExprKind::Old(_) => Some(super::BackendRequirement {
                requirement_id: "LANG-CONTRACT-001".into(),
                capability: "contract.old_snapshot".into(),
            }),
            ResolvedExprKind::Comptime(_) => Some(super::BackendRequirement {
                requirement_id: "COMPTIME-PURE-001".into(),
                capability: "comptime.evaluate".into(),
            }),
            ResolvedExprKind::Scope {
                kind: super::ResolvedScopeKind::Arena,
                ..
            } => Some(super::BackendRequirement {
                requirement_id: "RESOURCE-LINEAR-001".into(),
                capability: "allocator.arena".into(),
            }),
            _ => None,
        }
        .into_iter()
        .collect();
        Ok(ResolvedExpr {
            node_id,
            origin,
            ty,
            effects,
            backend_requirements,
            kind,
        })
    }

    fn lower_lambda(
        &mut self,
        node_id: &NodeId,
        params: &[Param],
        body: &[Stmt],
        role: &str,
        ty: &ResolvedTypeId,
    ) -> Result<ResolvedExprKind, Vec<ResolvedBodyError>> {
        let (parameter_types, result_type) = match self.types.get(ty) {
            Some(ResolvedType::Function {
                parameters, result, ..
            }) => (parameters.clone(), result.clone()),
            _ => {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "lambda expression does not have a canonical function type",
                )])
            }
        };
        if params.len() != parameter_types.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "lambda parameter count disagrees with its canonical function type",
            )]);
        }

        self.scopes.push(BTreeMap::new());
        self.lambda_contexts.push(LambdaCaptureContext {
            owned: BTreeSet::new(),
            captures: BTreeSet::new(),
        });
        let lowered = (|| {
            let mut parameters = Vec::with_capacity(params.len());
            for (parameter, parameter_type) in params.iter().zip(parameter_types) {
                if parameter.default_value.is_some() {
                    return Err(vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!(
                            "lambda parameter '{}' uses a default value outside the typed default model",
                            parameter.name
                        ),
                    )]);
                }
                let parameter_node = self.catalogued_id(
                    "decl.parameter",
                    &format!("{role}.parameter.{}", stable_id_fragment(&parameter.name)),
                    usable_span(parameter.meta.span),
                    parameter.meta.origin,
                )?;
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", parameter_node.0)));
                let origin = self.origin(&parameter_node)?;
                self.insert_local(
                    parameter.name.clone(),
                    ResolvedLocal {
                        id: local_id.clone(),
                        display_name: parameter.name.clone(),
                        ty: parameter_type,
                        mutable: parameter.mut_,
                        origin,
                    },
                    &parameter_node,
                )?;
                parameters.push(local_id);
            }
            let body = self.lower_block(body, &format!("{role}.body"), result_type, true)?;
            Ok::<_, Vec<ResolvedBodyError>>((parameters, body))
        })();
        let capture_context = self
            .lambda_contexts
            .pop()
            .expect("lambda lowering installed a capture context");
        self.scopes.pop();
        let (parameters, body) = lowered?;

        Ok(ResolvedExprKind::Lambda(Box::new(ResolvedLambda {
            owner: NodeId(format!("{}/callable", node_id.0)),
            parameters,
            captures: capture_context.captures.into_iter().collect(),
            body,
        })))
    }

    fn lower_record(
        &mut self,
        node_id: &NodeId,
        fields: &[crate::ast::RecordFieldExpr],
        role: &str,
        ty: &ResolvedTypeId,
    ) -> Result<ResolvedExprKind, Vec<ResolvedBodyError>> {
        let nominal = match self.types.get(ty) {
            Some(ResolvedType::Nominal { item, .. }) => item.clone(),
            _ => return self.unsupported(node_id, "record construction without nominal type"),
        };
        let owner = NodeId(nominal.as_str().to_string());
        let declared: Vec<String> = if let Some(definition) = self.type_defs.get(&owner) {
            if definition.kind != ResolvedTypeKind::Record {
                return self.unsupported(node_id, "non-record nominal construction");
            }
            definition
                .fields
                .iter()
                .map(|(name, _)| name.clone())
                .collect()
        } else if let Some(schema) = crate::core::resolved::builtin_record_schema(&owner.0) {
            schema.iter().map(|(name, _)| (*name).to_string()).collect()
        } else {
            let states = self
                .flows
                .values()
                .flat_map(|flow| flow.states.values())
                .filter(|state| state.node_id == owner)
                .collect::<Vec<_>>();
            let [state] = states.as_slice() else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("record owner '{}' has no unique field catalog", owner.0),
                )]);
            };
            state.payload.iter().map(|(name, _)| name.clone()).collect()
        };
        let mut surface = BTreeMap::new();
        for field in fields {
            if surface.insert(field.name.as_str(), field).is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("record field '{}' is supplied more than once", field.name),
                )]);
            }
        }
        let mut lowered = Vec::with_capacity(declared.len());
        for declaration in &declared {
            let value = surface.remove(declaration.as_str()).ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("record field '{declaration}' has no checked value"),
                )]
            })?;
            let field_id = self.resolve_field(node_id, ty, declaration)?;
            let declaration_ty = self.field_types.get(&field_id).ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    field_id.clone(),
                    "record field has no canonical declaration type",
                )]
            })?;
            let target_ty = self.instantiate_member_type(node_id, ty, declaration_ty)?;
            let value = self.lower_expr(
                &value.value,
                &format!("{role}.field.{}.value", stable_id_fragment(declaration)),
            )?;
            let conversion = self.implicit_conversion(node_id, &value.ty, &target_ty)?;
            lowered.push(ResolvedRecordField {
                field: field_id,
                value,
                conversion,
            });
        }
        if let Some(extra) = surface.keys().next() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("record field '{extra}' has no declaration"),
            )]);
        }
        Ok(ResolvedExprKind::Record {
            nominal,
            fields: lowered,
        })
    }

    fn lower_match_arms(
        &mut self,
        arms: &[crate::ast::MatchArm],
        role: &str,
        pattern_ty: &ResolvedTypeId,
        result_ty: &ResolvedTypeId,
    ) -> Result<Vec<ResolvedMatchArm>, Vec<ResolvedBodyError>> {
        let mut lowered = Vec::with_capacity(arms.len());
        for index in 0..arms.len() {
            let arm = &arms[index];
            let arm_role = match_arm_role(&format!("{role}.arm"), arms, index);
            let mut diagnostics = Vec::new();
            let node_id = self.ids.anonymous(
                &self.owner,
                "match.arm",
                &arm_role,
                usable_span(arm.meta.span),
                arm.meta.origin,
                &mut diagnostics,
            );
            if !diagnostics.is_empty() || !self.node_meta.contains_key(&node_id) {
                return Err(vec![ResolvedBodyError::new(
                    node_id,
                    "match arm has no stable semantic identity",
                )]);
            }
            let origin = self.origin(&node_id)?;
            self.scopes.push(BTreeMap::new());
            let arm_result = (|| {
                let pattern = self.lower_binding_pattern(
                    &arm.pat,
                    &format!("{arm_role}.pattern"),
                    pattern_ty.clone(),
                    false,
                )?;
                let guard = arm
                    .guard
                    .as_ref()
                    .map(|guard| self.lower_expr(guard, &format!("{arm_role}.guard")))
                    .transpose()?;
                let body = self.lower_expr(&arm.body, &format!("{arm_role}.body"))?;
                if &body.ty != result_ty {
                    return Err(vec![ResolvedBodyError::new(
                        body.node_id.clone(),
                        "match arm body type disagrees with match result type",
                    )]);
                }
                Ok(ResolvedMatchArm {
                    node_id,
                    origin,
                    pattern,
                    guard,
                    body,
                })
            })();
            self.scopes.pop();
            lowered.push(arm_result?);
        }
        Ok(lowered)
    }

    fn lower_call(
        &mut self,
        node_id: &NodeId,
        callee: &Expr,
        arguments: &[Expr],
        role: &str,
        type_arguments: &[ResolvedTypeId],
    ) -> Result<ResolvedCall, Vec<ResolvedBodyError>> {
        let site = self.call_sites.get(node_id).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "call has no checker-resolved call-site record",
            )]
        })?;
        if let Expr::Field(flow, event) = callee.unlocated() {
            if let Expr::Ident(flow) = flow.unlocated() {
                if self.has_transition_callee(flow, event) {
                    return self.lower_transition_call(
                        node_id,
                        flow,
                        event,
                        arguments,
                        role,
                        type_arguments,
                    );
                }
                if matches!(event.as_str(), "spawn" | "spawn_detached") {
                    if let Some(call) = self.lower_actor_spawn_call(
                        node_id,
                        flow,
                        event,
                        arguments,
                        type_arguments,
                    )? {
                        return Ok(call);
                    }
                }
            }
        }
        if site.kind == ResolvedCallKind::Unknown {
            if matches!(callee.unlocated(), Expr::Field(_, _)) {
                if let Some(call) = self.lower_builtin_method_call(
                    node_id,
                    callee,
                    arguments,
                    role,
                    type_arguments,
                )? {
                    return Ok(call);
                }
            }
            if let Expr::Ident(name) = callee.unlocated() {
                if let Some(call) =
                    self.lower_variant_constructor_call(node_id, name, arguments, role)?
                {
                    return Ok(call);
                }
                if let Some(call) =
                    self.lower_type_constructor_call(node_id, name, arguments, role)?
                {
                    return Ok(call);
                }
                if let Some(local) = self.lookup_local(name) {
                    return self.lower_local_closure_call(
                        node_id,
                        local,
                        arguments,
                        role,
                        type_arguments,
                    );
                }
                if let Some(function) =
                    self.function
                        .body
                        .iter()
                        .find_map(|statement| match statement.unlocated() {
                            Stmt::Func(function) if function.name == *name => {
                                Some(function.clone())
                            }
                            _ => None,
                        })
                {
                    return self.lower_nested_function_call(
                        node_id,
                        &function,
                        arguments,
                        role,
                        type_arguments,
                    );
                }
            }
        }
        if site.kind == ResolvedCallKind::Builtin {
            if !type_arguments.is_empty() && site.callee != "from_json" {
                return self.unsupported(node_id, "generic arguments on builtin call");
            }
            let result = self.expression_type(node_id)?;
            if site.callee == "from_json"
                && (type_arguments.len() != 1 || type_arguments.first() != Some(&result))
            {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "from_json type argument disagrees with checker-finalized result type",
                )]);
            }
            let builtin =
                super::BuiltinId::new(site.callee.clone()).map_err(|error| vec![error])?;
            let mut lowered = Vec::with_capacity(arguments.len());
            for index in 0..arguments.len() {
                if matches!(arguments[index].unlocated(), Expr::NamedArg(_, _)) {
                    return self.unsupported(node_id, "named arguments for builtin call");
                }
                let argument_role =
                    expr_sibling_role(&format!("{role}.argument"), arguments, index);
                let value = self.lower_expr(&arguments[index], &argument_role)?;
                lowered.push(ResolvedArgument {
                    parameter: super::ResolvedParameterId(NodeId(format!(
                        "builtin:{}/parameter:{index}",
                        site.callee
                    ))),
                    conversion: CheckedConversion {
                        kind: CheckedConversionKind::Identity,
                        from: value.ty.clone(),
                        to: value.ty.clone(),
                    },
                    value,
                });
            }
            return Ok(ResolvedCall {
                callee: ResolvedCallee::Builtin(builtin),
                result,
                type_arguments: type_arguments.to_vec(),
                arguments: lowered,
                permission: None,
                effects: Vec::new(),
                session: Vec::new(),
            });
        }
        if site.kind == ResolvedCallKind::Extern {
            if !type_arguments.is_empty() {
                return self.unsupported(node_id, "generic arguments on extern call");
            }
            let candidates = self
                .extern_blocks
                .values()
                .flat_map(|block| block.signatures.iter())
                .filter(|function| function.name == site.callee)
                .collect::<Vec<_>>();
            let [function] = candidates.as_slice() else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "extern call '{}' does not resolve to exactly one declaration",
                        site.callee
                    ),
                )]);
            };
            let arity_valid = if function.variadic {
                arguments.len() >= function.parameter_ids.len()
            } else {
                arguments.len() == function.parameter_ids.len()
            };
            if !arity_valid {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "extern argument count disagrees with canonical declaration",
                )]);
            }
            let mut lowered = Vec::with_capacity(arguments.len());
            for index in 0..arguments.len() {
                if matches!(arguments[index].unlocated(), Expr::NamedArg(_, _)) {
                    return self.unsupported(node_id, "named arguments for extern call");
                }
                let argument_role =
                    expr_sibling_role(&format!("{role}.argument"), arguments, index);
                let value = self.lower_expr(&arguments[index], &argument_role)?;
                let parameter = function
                    .parameter_ids
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| NodeId(format!("{}/variadic:{index}", function.node_id.0)));
                lowered.push(ResolvedArgument {
                    parameter: super::ResolvedParameterId(parameter),
                    conversion: CheckedConversion {
                        kind: CheckedConversionKind::Identity,
                        from: value.ty.clone(),
                        to: value.ty.clone(),
                    },
                    value,
                });
            }
            return Ok(ResolvedCall {
                callee: ResolvedCallee::Extern(function.node_id.clone()),
                result: self.expression_type(node_id)?,
                type_arguments: Vec::new(),
                arguments: lowered,
                permission: None,
                effects: Vec::new(),
                session: Vec::new(),
            });
        }
        if site.kind == ResolvedCallKind::Method {
            if !type_arguments.is_empty() {
                return self.unsupported(node_id, "generic arguments on method call");
            }
            return self.lower_method_call(node_id, callee, arguments, role);
        }
        if site.kind != ResolvedCallKind::Function {
            return self.unsupported(node_id, &format!("closed {:?} call target", site.kind));
        }
        let mut candidates = self
            .functions
            .values()
            .filter(|function| function.qualified_name == site.callee)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let [function] = candidates.as_slice() else {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "function call '{}' does not resolve to exactly one callable identity",
                    site.callee
                ),
            )]);
        };
        let signature = self.signatures.get(&function.node_id).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("callee '{}' has no canonical signature", function.node_id.0),
            )]
        })?;
        if !type_arguments.is_empty() && signature.generic_parameters.len() != type_arguments.len()
        {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "call supplies {} generic arguments but canonical signature requires {}",
                    type_arguments.len(),
                    signature.generic_parameters.len()
                ),
            )]);
        }
        let mut substitutions = signature
            .generic_parameters
            .iter()
            .cloned()
            .zip(type_arguments.iter().cloned())
            .collect::<BTreeMap<_, _>>();
        if arguments.len() > signature.parameters.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "call argument count {} exceeds canonical parameter count {}",
                    arguments.len(),
                    signature.parameters.len()
                ),
            )]);
        }

        let mut slots = vec![None; signature.parameters.len()];
        let mut next_positional = 0;
        for index in 0..arguments.len() {
            let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, index);
            let (slot, value, value_role) = match arguments[index].unlocated() {
                Expr::NamedArg(name, value) => {
                    let slot = signature
                        .parameters
                        .iter()
                        .position(|parameter| parameter.name == *name)
                        .ok_or_else(|| {
                            vec![ResolvedBodyError::new(
                                node_id.clone(),
                                format!("named argument '{name}' has no canonical parameter"),
                            )]
                        })?;
                    (slot, value.as_ref(), format!("{argument_role}.inner"))
                }
                _ => {
                    while next_positional < slots.len() && slots[next_positional].is_some() {
                        next_positional += 1;
                    }
                    let slot = next_positional;
                    next_positional += 1;
                    (slot, &arguments[index], argument_role)
                }
            };
            if slots[slot].replace((value, value_role)).is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "parameter '{}' is supplied more than once",
                        signature.parameters[slot].name
                    ),
                )]);
            }
        }

        let mut lowered = Vec::with_capacity(slots.len());
        for (parameter, slot) in signature.parameters.iter().zip(slots) {
            let value = if let Some((value, value_role)) = slot {
                self.lower_expr(value, &value_role)?
            } else if parameter.has_default {
                if !signature.generic_parameters.is_empty() {
                    return self.unsupported(node_id, "generic parameter default instantiation");
                }
                let declaration = function
                    .param_decls
                    .iter()
                    .find(|declaration| declaration.name == parameter.name)
                    .ok_or_else(|| {
                        vec![ResolvedBodyError::new(
                            node_id.clone(),
                            format!(
                                "defaulted parameter '{}' has no declaration",
                                parameter.name
                            ),
                        )]
                    })?;
                if declaration.default_value.is_none() {
                    return Err(vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!(
                            "parameter '{}' is marked defaulted without a typed default body",
                            parameter.name
                        ),
                    )]);
                }
                ResolvedExpr {
                    node_id: NodeId(format!(
                        "{}/default-argument:{}",
                        node_id.0,
                        stable_id_fragment(&parameter.name)
                    )),
                    origin: Origin::Desugared {
                        parent: node_id.clone(),
                        rule: "resolved_body.default_argument".into(),
                        span: self.origin(node_id)?.user_span(),
                    },
                    ty: parameter.ty.clone(),
                    effects: Vec::new(),
                    backend_requirements: Vec::new(),
                    kind: ResolvedExprKind::DefaultArgument {
                        callable: function.node_id.clone(),
                        parameter: parameter.id.clone(),
                    },
                }
            } else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("parameter '{}' has no checked argument", parameter.name),
                )]);
            };
            let target = if signature.generic_parameters.is_empty() {
                parameter.ty.clone()
            } else if self.collect_instantiation(&parameter.ty, &value.ty, &mut substitutions) {
                value.ty.clone()
            } else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "argument for generic parameter '{}' disagrees with explicit instantiation",
                        parameter.name
                    ),
                )]);
            };
            let conversion = self.implicit_conversion(node_id, &value.ty, &target)?;
            lowered.push(ResolvedArgument {
                parameter: parameter.id.clone(),
                value,
                conversion,
            });
        }
        let call_result = self.node_types.get(node_id).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "call has no checker-finalized result type",
            )]
        })?;
        if !self.collect_instantiation(&signature.result, call_result, &mut substitutions) {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "call result type disagrees with canonical generic instantiation",
            )]);
        }
        let type_arguments = signature
            .generic_parameters
            .iter()
            .map(|parameter| {
                substitutions.get(parameter).cloned().ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!(
                            "generic parameter '{}' has no checker-closed instantiation",
                            parameter.0
                        ),
                    )]
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let effects = site
            .effects
            .iter()
            .map(|effect| {
                EffectId::new(effect.clone()).map_err(|error| {
                    vec![ResolvedBodyError::new(node_id.clone(), error.to_string())]
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedCall {
            callee: ResolvedCallee::Function(function.node_id.clone()),
            result: call_result.clone(),
            type_arguments,
            arguments: lowered,
            permission: None,
            effects,
            session: Vec::new(),
        })
    }

    fn has_transition_callee(&self, flow: &str, event: &str) -> bool {
        self.signatures.keys().any(|owner| {
            parse_transition_owner(owner).is_some_and(|(candidate_flow, candidate_event, _)| {
                candidate_event == event
                    && (candidate_flow == flow
                        || candidate_flow
                            .rsplit_once("::")
                            .is_some_and(|(_, short)| short == flow))
            })
        })
    }

    fn lower_transition_call(
        &mut self,
        node_id: &NodeId,
        flow: &str,
        event: &str,
        arguments: &[Expr],
        role: &str,
        type_arguments: &[ResolvedTypeId],
    ) -> Result<ResolvedCall, Vec<ResolvedBodyError>> {
        if !type_arguments.is_empty() {
            return self.unsupported(node_id, "generic arguments on transition call");
        }
        let Some(source_argument) = arguments.first() else {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "transition call has no source-state argument",
            )]);
        };
        if matches!(source_argument.unlocated(), Expr::NamedArg(_, _)) {
            return self.unsupported(node_id, "named transition source argument");
        }
        let source_role = expr_sibling_role(&format!("{role}.argument"), arguments, 0);
        let source = self.lower_expr(source_argument, &source_role)?;
        let mut candidates = self
            .signatures
            .iter()
            .filter_map(|(owner, signature)| {
                let (candidate_flow, candidate_event, source_state) =
                    parse_transition_owner(owner)?;
                let flow_matches = candidate_flow == flow
                    || candidate_flow
                        .rsplit_once("::")
                        .is_some_and(|(_, short)| short == flow);
                (flow_matches
                    && candidate_event == event
                    && signature
                        .parameters
                        .first()
                        .is_some_and(|parameter| parameter.ty == source.ty))
                .then_some((
                    owner.clone(),
                    signature.clone(),
                    candidate_flow.to_string(),
                    source_state.to_string(),
                ))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        let [(_owner, signature, qualified_flow, source_state)] = candidates.as_slice() else {
            let available = self
                .signatures
                .iter()
                .filter_map(|(owner, signature)| {
                    let (candidate_flow, candidate_event, source_state) =
                        parse_transition_owner(owner)?;
                    (candidate_event == event
                        && (candidate_flow == flow
                            || candidate_flow
                                .rsplit_once("::")
                                .is_some_and(|(_, short)| short == flow)))
                    .then(|| {
                        format!(
                            "{}:{}",
                            source_state,
                            signature
                                .parameters
                                .first()
                                .map(|parameter| parameter.ty.as_str())
                                .unwrap_or("missing")
                        )
                    })
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "transition call '{flow}::{event}' with source '{}' does not resolve to exactly one source-state overload (available: {available})",
                    source.ty.as_str()
                ),
            )]);
        };
        if arguments.len() != signature.parameters.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "transition argument count {} does not match canonical parameter count {}",
                    arguments.len(),
                    signature.parameters.len()
                ),
            )]);
        }

        let mut slots = vec![None; signature.parameters.len()];
        slots[0] = Some((source, source_role));
        let mut next_positional = 1;
        for index in 1..arguments.len() {
            let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, index);
            let (slot, value, value_role) = match arguments[index].unlocated() {
                Expr::NamedArg(name, value) => {
                    let slot = signature
                        .parameters
                        .iter()
                        .position(|parameter| parameter.name == *name)
                        .ok_or_else(|| {
                            vec![ResolvedBodyError::new(
                                node_id.clone(),
                                format!("named transition argument '{name}' has no parameter"),
                            )]
                        })?;
                    (slot, value.as_ref(), format!("{argument_role}.inner"))
                }
                _ => {
                    while next_positional < slots.len() && slots[next_positional].is_some() {
                        next_positional += 1;
                    }
                    let slot = next_positional;
                    next_positional += 1;
                    (slot, &arguments[index], argument_role)
                }
            };
            if slot == 0 || slots[slot].is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "transition parameter '{}' is supplied more than once",
                        signature.parameters[slot].name
                    ),
                )]);
            }
            let value = self.lower_expr(value, &value_role)?;
            slots[slot] = Some((value, value_role));
        }

        let mut lowered = Vec::with_capacity(slots.len());
        for (parameter, slot) in signature.parameters.iter().zip(slots) {
            let Some((value, _)) = slot else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("transition parameter '{}' has no argument", parameter.name),
                )]);
            };
            let conversion = self.implicit_conversion(node_id, &value.ty, &parameter.ty)?;
            lowered.push(ResolvedArgument {
                parameter: parameter.id.clone(),
                value,
                conversion,
            });
        }
        let call_result = self.node_types.get(node_id).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "transition call has no checker-finalized result type",
            )]
        })?;
        if signature.result != *call_result
            && !self.is_system_fault_refinement(&signature.result, call_result)
        {
            self.identity_conversion(node_id, &signature.result, call_result)?;
        }
        let flow_id = crate::core::FlowId(qualified_flow.clone());
        Ok(ResolvedCall {
            callee: ResolvedCallee::Transition(crate::core::TransitionId {
                flow: flow_id.clone(),
                event: event.to_string(),
                source: crate::core::StateId {
                    flow: flow_id,
                    name: source_state.clone(),
                },
            }),
            result: signature.result.clone(),
            type_arguments: Vec::new(),
            arguments: lowered,
            permission: Some(super::Permission::Consume),
            effects: signature.effects.clone(),
            session: Vec::new(),
        })
    }

    fn lower_local_closure_call(
        &mut self,
        node_id: &NodeId,
        local: ResolvedLocalId,
        arguments: &[Expr],
        role: &str,
        type_arguments: &[ResolvedTypeId],
    ) -> Result<ResolvedCall, Vec<ResolvedBodyError>> {
        if !type_arguments.is_empty() {
            return self.unsupported(node_id, "generic arguments on local closure call");
        }
        let local_type = self
            .locals
            .get(&local)
            .map(|local| local.ty.clone())
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "local closure call references a missing local",
                )]
            })?;
        let (parameters, result) = match self.types.get(&local_type) {
            Some(ResolvedType::Function {
                parameters, result, ..
            }) => (parameters.clone(), result.clone()),
            _ => {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("local '{}' is not callable", local.0 .0),
                )])
            }
        };
        if arguments.len() != parameters.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "local closure argument count {} does not match canonical parameter count {}",
                    arguments.len(),
                    parameters.len()
                ),
            )]);
        }
        let call_result = self.node_types.get(node_id).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "local closure call has no checker-finalized result type",
            )]
        })?;
        // A let-bound callable may carry a generalized HM scheme. The local's
        // initializer type is one canonical representative, while each call
        // expression records its independently instantiated result and
        // argument types. The checker-finalized call-site types are therefore
        // the authority here; consumers need no generic substitution.
        let polymorphic = result != *call_result;

        let mut lowered = Vec::with_capacity(arguments.len());
        for (index, (argument, parameter_type)) in
            arguments.iter().zip(parameters.into_iter()).enumerate()
        {
            if matches!(argument.unlocated(), Expr::NamedArg(_, _)) {
                return self.unsupported(node_id, "named arguments on local closure call");
            }
            let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, index);
            let value = self.lower_expr(argument, &argument_role)?;
            let conversion = if polymorphic || value.ty != parameter_type {
                CheckedConversion {
                    kind: CheckedConversionKind::Identity,
                    from: value.ty.clone(),
                    to: value.ty.clone(),
                }
            } else {
                self.identity_conversion(node_id, &value.ty, &parameter_type)?
            };
            lowered.push(ResolvedArgument {
                parameter: super::ResolvedParameterId(NodeId(format!(
                    "{}/call-parameter:{index}",
                    local.0 .0
                ))),
                value,
                conversion,
            });
        }
        Ok(ResolvedCall {
            callee: ResolvedCallee::LocalClosure(local),
            result: call_result.clone(),
            type_arguments: Vec::new(),
            arguments: lowered,
            permission: None,
            effects: Vec::new(),
            session: Vec::new(),
        })
    }

    fn lower_nested_function_call(
        &mut self,
        node_id: &NodeId,
        function: &FuncDef,
        arguments: &[Expr],
        role: &str,
        type_arguments: &[ResolvedTypeId],
    ) -> Result<ResolvedCall, Vec<ResolvedBodyError>> {
        let owner = nested_function_owner(&self.owner, function);
        let signature = self.signatures.get(&owner).cloned().ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("nested callable '{}' has no canonical signature", owner.0),
            )]
        })?;
        if !type_arguments.is_empty() && type_arguments.len() != signature.generic_parameters.len()
        {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "nested call supplies {} type arguments for {} generic parameters",
                    type_arguments.len(),
                    signature.generic_parameters.len()
                ),
            )]);
        }
        if arguments.len() > signature.parameters.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "nested call has more arguments than canonical parameters",
            )]);
        }
        let mut substitutions = signature
            .generic_parameters
            .iter()
            .cloned()
            .zip(type_arguments.iter().cloned())
            .collect::<BTreeMap<_, _>>();
        let mut slots = vec![None; signature.parameters.len()];
        let mut next_positional = 0;
        for index in 0..arguments.len() {
            let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, index);
            let (slot, value, value_role) = match arguments[index].unlocated() {
                Expr::NamedArg(name, value) => {
                    let slot = signature
                        .parameters
                        .iter()
                        .position(|parameter| parameter.name == *name)
                        .ok_or_else(|| {
                            vec![ResolvedBodyError::new(
                                node_id.clone(),
                                format!("nested argument '{name}' has no parameter"),
                            )]
                        })?;
                    (slot, value.as_ref(), format!("{argument_role}.inner"))
                }
                _ => {
                    while next_positional < slots.len() && slots[next_positional].is_some() {
                        next_positional += 1;
                    }
                    let slot = next_positional;
                    next_positional += 1;
                    (slot, &arguments[index], argument_role)
                }
            };
            if slots[slot].is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "nested parameter '{}' is supplied more than once",
                        signature.parameters[slot].name
                    ),
                )]);
            }
            slots[slot] = Some((value, value_role));
        }
        let mut lowered = Vec::with_capacity(slots.len());
        for (parameter, slot) in signature.parameters.iter().zip(slots) {
            let value = if let Some((value, value_role)) = slot {
                self.lower_expr(value, &value_role)?
            } else if parameter.has_default {
                if !signature.generic_parameters.is_empty() {
                    return self.unsupported(node_id, "generic nested callable default argument");
                }
                ResolvedExpr {
                    node_id: NodeId(format!(
                        "{}/default-argument:{}",
                        node_id.0,
                        stable_id_fragment(&parameter.name)
                    )),
                    origin: Origin::Desugared {
                        parent: node_id.clone(),
                        rule: "resolved_body.default_argument".into(),
                        span: self.origin(node_id)?.user_span(),
                    },
                    ty: parameter.ty.clone(),
                    effects: Vec::new(),
                    backend_requirements: Vec::new(),
                    kind: ResolvedExprKind::DefaultArgument {
                        callable: owner.clone(),
                        parameter: parameter.id.clone(),
                    },
                }
            } else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("nested parameter '{}' has no argument", parameter.name),
                )]);
            };
            let target = if signature.generic_parameters.is_empty() {
                parameter.ty.clone()
            } else if self.collect_instantiation(&parameter.ty, &value.ty, &mut substitutions) {
                value.ty.clone()
            } else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "nested argument for '{}' disagrees with its generic instantiation",
                        parameter.name
                    ),
                )]);
            };
            lowered.push(ResolvedArgument {
                parameter: parameter.id.clone(),
                conversion: self.implicit_conversion(node_id, &value.ty, &target)?,
                value,
            });
        }
        let result = self.expression_type(node_id)?;
        if !self.collect_instantiation(&signature.result, &result, &mut substitutions) {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "nested call result disagrees with its canonical instantiation",
            )]);
        }
        let type_arguments = signature
            .generic_parameters
            .iter()
            .map(|parameter| {
                substitutions.get(parameter).cloned().ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!(
                            "nested generic parameter '{}' has no closed instantiation",
                            parameter.0
                        ),
                    )]
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedCall {
            callee: ResolvedCallee::Function(owner),
            result,
            type_arguments,
            arguments: lowered,
            permission: None,
            effects: signature.effects.clone(),
            session: Vec::new(),
        })
    }

    fn lower_type_constructor_call(
        &mut self,
        node_id: &NodeId,
        name: &str,
        arguments: &[Expr],
        role: &str,
    ) -> Result<Option<ResolvedCall>, Vec<ResolvedBodyError>> {
        let mut definitions = self
            .type_defs
            .values()
            .filter(|definition| {
                definition.kind == ResolvedTypeKind::Newtype
                    && (definition.qualified_name == name
                        || definition
                            .qualified_name
                            .rsplit_once("::")
                            .is_some_and(|(_, short)| short == name))
            })
            .collect::<Vec<_>>();
        definitions.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let [definition] = definitions.as_slice() else {
            return Ok(None);
        };
        let definition_id = definition.node_id.clone();
        if arguments.len() != 1 || matches!(arguments[0].unlocated(), Expr::NamedArg(_, _)) {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("newtype constructor '{name}' requires one positional argument"),
            )]);
        }
        let result = self.expression_type(node_id)?;
        let target = self
            .instantiated_type_target(node_id, &result)?
            .filter(|(kind, _)| *kind == ResolvedTypeKind::Newtype)
            .map(|(_, target)| target)
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("newtype constructor '{name}' has no instantiated target"),
                )]
            })?;
        let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, 0);
        let value = self.lower_expr(&arguments[0], &argument_role)?;
        let conversion = self.implicit_conversion(node_id, &value.ty, &target)?;
        Ok(Some(ResolvedCall {
            callee: ResolvedCallee::Constructor(definition_id.clone()),
            result,
            type_arguments: Vec::new(),
            arguments: vec![ResolvedArgument {
                parameter: super::ResolvedParameterId(NodeId(format!(
                    "{}/constructor-parameter",
                    definition_id.0
                ))),
                value,
                conversion,
            }],
            permission: None,
            effects: Vec::new(),
            session: Vec::new(),
        }))
    }

    fn lower_variant_constructor_call(
        &mut self,
        node_id: &NodeId,
        name: &str,
        arguments: &[Expr],
        role: &str,
    ) -> Result<Option<ResolvedCall>, Vec<ResolvedBodyError>> {
        let result = self.expression_type(node_id)?;
        let (owner, type_arguments) = match self.types.get(&result) {
            Some(ResolvedType::Nominal { item, arguments }) => {
                (NodeId(item.as_str().to_string()), arguments.clone())
            }
            _ => return Ok(None),
        };
        let Some(definition) = self.type_defs.get(&owner) else {
            return Ok(None);
        };
        let Some((variant_id, declared)) =
            self.instantiated_variant_fields(node_id, &result, definition, name)?
        else {
            return Ok(None);
        };
        if arguments.len() != declared.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "variant constructor '{name}' expects {} arguments, got {}",
                    declared.len(),
                    arguments.len()
                ),
            )]);
        }
        let mut slots = vec![None; declared.len()];
        let mut next_positional = 0;
        for (index, argument) in arguments.iter().enumerate() {
            let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, index);
            let (slot, value, value_role) = match argument.unlocated() {
                Expr::NamedArg(name, value) => {
                    let slot = declared
                        .iter()
                        .position(|(field, _, _)| field == name)
                        .ok_or_else(|| {
                            vec![ResolvedBodyError::new(
                                node_id.clone(),
                                format!("variant constructor has no field '{name}'"),
                            )]
                        })?;
                    (slot, value.as_ref(), format!("{argument_role}.inner"))
                }
                _ => {
                    while next_positional < slots.len() && slots[next_positional].is_some() {
                        next_positional += 1;
                    }
                    let slot = next_positional;
                    next_positional += 1;
                    (slot, argument, argument_role)
                }
            };
            if slots[slot].replace((value, value_role)).is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "variant constructor field '{}' is supplied twice",
                        declared[slot].0
                    ),
                )]);
            }
        }
        let mut lowered = Vec::with_capacity(declared.len());
        for ((name, field, target), slot) in declared.iter().zip(slots) {
            let (value, value_role) = slot.ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("variant constructor field '{name}' has no argument"),
                )]
            })?;
            let value = self.lower_expr(value, &value_role)?;
            let conversion = self.implicit_conversion(node_id, &value.ty, target)?;
            lowered.push(ResolvedArgument {
                parameter: super::ResolvedParameterId(field.clone()),
                value,
                conversion,
            });
        }
        Ok(Some(ResolvedCall {
            callee: ResolvedCallee::Constructor(variant_id),
            result,
            type_arguments,
            arguments: lowered,
            permission: None,
            effects: Vec::new(),
            session: Vec::new(),
        }))
    }

    fn lower_actor_spawn_call(
        &mut self,
        node_id: &NodeId,
        actor_name: &str,
        operation: &str,
        arguments: &[Expr],
        explicit_type_arguments: &[ResolvedTypeId],
    ) -> Result<Option<ResolvedCall>, Vec<ResolvedBodyError>> {
        let mut actors = self
            .actors
            .values()
            .filter(|actor| {
                actor.qualified_name == actor_name
                    || actor
                        .qualified_name
                        .rsplit_once("::")
                        .is_some_and(|(_, short)| short == actor_name)
            })
            .collect::<Vec<_>>();
        actors.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let [actor] = actors.as_slice() else {
            return Ok(None);
        };
        if !arguments.is_empty() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("actor {operation} does not accept constructor arguments"),
            )]);
        }
        if !explicit_type_arguments.is_empty() {
            return self.unsupported(node_id, "generic arguments on actor spawn");
        }
        let result = self.expression_type(node_id)?;
        match self.types.get(&result) {
            Some(ResolvedType::Nominal { item, .. }) if item.as_str() == actor.node_id.0 => {}
            _ => {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "actor {operation} result does not identify actor '{}'",
                        actor.node_id.0
                    ),
                )])
            }
        }
        Ok(Some(ResolvedCall {
            callee: ResolvedCallee::Builtin(
                super::BuiltinId::new(format!("actor.{operation}")).map_err(|error| vec![error])?,
            ),
            result: result.clone(),
            type_arguments: vec![result],
            arguments: Vec::new(),
            permission: None,
            effects: Vec::new(),
            session: Vec::new(),
        }))
    }

    fn lower_method_call(
        &mut self,
        node_id: &NodeId,
        callee: &Expr,
        arguments: &[Expr],
        role: &str,
    ) -> Result<ResolvedCall, Vec<ResolvedBodyError>> {
        let Expr::Field(receiver, method_name) = callee.unlocated() else {
            return self.unsupported(node_id, "method call without a receiver projection");
        };
        let receiver = self.lower_expr(receiver, &format!("{role}.callee.inner"))?;
        let actor_id = match self.types.get(&receiver.ty) {
            Some(ResolvedType::Nominal { item, .. }) => {
                let item = NodeId(item.as_str().to_string());
                self.actors.contains_key(&item).then_some(item)
            }
            _ => None,
        };
        let (function_id, resolved_callee, mut substitutions) = if let Some(actor_id) = actor_id {
            let actor = &self.actors[&actor_id];
            let function_id = NodeId(format!(
                "function:{}::{}",
                actor.qualified_name, method_name
            ));
            if !self.functions.contains_key(&function_id) {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "actor method '{}::{}' has no callable identity",
                        actor.qualified_name, method_name
                    ),
                )]);
            }
            let method =
                super::MethodId::new(function_id.0.clone()).map_err(|error| vec![error])?;
            (
                function_id,
                ResolvedCallee::ActorMethod {
                    actor: actor_id,
                    method,
                },
                BTreeMap::new(),
            )
        } else {
            let mut candidates = Vec::new();
            for impl_def in self.impls.values() {
                if !impl_def.methods.iter().any(|name| name == method_name) {
                    continue;
                }
                let prefix = format!("function:{}::{}:", impl_def.qualified_name, method_name);
                for function in self
                    .functions
                    .values()
                    .filter(|function| function.node_id.0.starts_with(&prefix))
                {
                    let Some(signature) = self.signatures.get(&function.node_id) else {
                        continue;
                    };
                    if let Some(parameter) = signature.parameters.first() {
                        let mut substitutions = BTreeMap::new();
                        if self.collect_instantiation(
                            &parameter.ty,
                            &receiver.ty,
                            &mut substitutions,
                        ) {
                            candidates.push((impl_def, function, substitutions));
                        }
                    }
                }
            }
            candidates.sort_by(|(_, left, _), (_, right, _)| left.node_id.cmp(&right.node_id));
            let [(impl_def, function, substitutions)] = candidates.as_slice() else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "method '{method_name}' does not resolve to exactly one impl callable for receiver type '{}'",
                        receiver.ty.as_str()
                    ),
                )]);
            };
            let mut protocols = self
                .traits
                .values()
                .filter(|trait_def| {
                    trait_def.qualified_name == impl_def.trait_name
                        || trait_def
                            .qualified_name
                            .rsplit_once("::")
                            .is_some_and(|(_, short)| short == impl_def.trait_name)
                })
                .collect::<Vec<_>>();
            protocols.sort_by(|left, right| left.node_id.cmp(&right.node_id));
            let [protocol] = protocols.as_slice() else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "impl '{}' does not resolve to exactly one protocol identity",
                        impl_def.qualified_name
                    ),
                )]);
            };
            let function_id = function.node_id.clone();
            let method =
                super::MethodId::new(function_id.0.clone()).map_err(|error| vec![error])?;
            (
                function_id,
                ResolvedCallee::ProtocolMethod {
                    protocol: protocol.node_id.clone(),
                    method,
                },
                substitutions.clone(),
            )
        };
        let signature = self.signatures.get(&function_id).cloned().ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("method '{}' has no canonical signature", function_id.0),
            )]
        })?;
        let Some(receiver_parameter) = signature.parameters.first() else {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "method signature has no receiver parameter",
            )]);
        };
        if receiver_parameter.name != "self" {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "method receiver parameter is not canonical self",
            )]);
        }
        let explicit_parameters = &signature.parameters[1..];
        if arguments.len() != explicit_parameters.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "method call argument count {} does not match canonical explicit parameter count {}",
                    arguments.len(),
                    explicit_parameters.len()
                ),
            )]);
        }

        let receiver_target =
            self.substitute_member_type(node_id, &receiver_parameter.ty, &substitutions)?;
        let receiver_conversion =
            self.identity_conversion(node_id, &receiver.ty, &receiver_target)?;
        let mut lowered = vec![ResolvedArgument {
            parameter: receiver_parameter.id.clone(),
            value: receiver,
            conversion: receiver_conversion,
        }];
        let mut slots = vec![None; explicit_parameters.len()];
        let mut next_positional = 0;
        for index in 0..arguments.len() {
            let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, index);
            let (slot, value, value_role) = match arguments[index].unlocated() {
                Expr::NamedArg(name, value) => {
                    let slot = explicit_parameters
                        .iter()
                        .position(|parameter| parameter.name == *name)
                        .ok_or_else(|| {
                            vec![ResolvedBodyError::new(
                                node_id.clone(),
                                format!("named argument '{name}' has no canonical parameter"),
                            )]
                        })?;
                    (slot, value.as_ref(), format!("{argument_role}.inner"))
                }
                _ => {
                    while next_positional < slots.len() && slots[next_positional].is_some() {
                        next_positional += 1;
                    }
                    let slot = next_positional;
                    next_positional += 1;
                    (slot, &arguments[index], argument_role)
                }
            };
            if slots[slot].replace((value, value_role)).is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "parameter '{}' is supplied more than once",
                        explicit_parameters[slot].name
                    ),
                )]);
            }
        }
        for (parameter, slot) in explicit_parameters.iter().zip(slots) {
            let (value, value_role) = slot.ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("parameter '{}' has no checked argument", parameter.name),
                )]
            })?;
            let value = self.lower_expr(value, &value_role)?;
            let mut inferred = substitutions.clone();
            if self.collect_instantiation(&parameter.ty, &value.ty, &mut inferred) {
                substitutions = inferred;
            }
            let parameter_ty =
                self.substitute_member_type(node_id, &parameter.ty, &substitutions)?;
            let conversion = self.implicit_conversion(node_id, &value.ty, &parameter_ty)?;
            lowered.push(ResolvedArgument {
                parameter: parameter.id.clone(),
                value,
                conversion,
            });
        }
        let type_arguments = signature
            .generic_parameters
            .iter()
            .map(|parameter| {
                substitutions.get(parameter).cloned().ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!(
                            "generic method parameter '{}' has no checker-closed instantiation",
                            parameter.0
                        ),
                    )]
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = self.substitute_member_type(node_id, &signature.result, &substitutions)?;
        let checked_result = self.expression_type(node_id)?;
        if result != checked_result {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "instantiated method result disagrees with checker-finalized expression type",
            )]);
        }
        let permission = receiver_parameter.permission;
        let effects = signature.effects.clone();
        Ok(ResolvedCall {
            callee: resolved_callee,
            result,
            type_arguments,
            arguments: lowered,
            permission,
            effects,
            session: Vec::new(),
        })
    }

    fn lower_builtin_method_call(
        &mut self,
        node_id: &NodeId,
        callee: &Expr,
        arguments: &[Expr],
        role: &str,
        explicit_type_arguments: &[ResolvedTypeId],
    ) -> Result<Option<ResolvedCall>, Vec<ResolvedBodyError>> {
        let Expr::Field(receiver, method_name) = callee.unlocated() else {
            return Ok(None);
        };
        let receiver = self.lower_expr(receiver, &format!("{role}.callee.inner"))?;
        let Some(method) =
            crate::core::builtins::resolve_builtin_method(&receiver.ty, method_name, self.types)
        else {
            return Ok(None);
        };
        if !explicit_type_arguments.is_empty() {
            return self.unsupported(
                node_id,
                "generic arguments on language-provided method call",
            );
        }
        let builtin =
            super::BuiltinId::new(method.identity.clone()).map_err(|error| vec![error])?;
        let receiver_parameter =
            super::ResolvedParameterId(NodeId(format!("{}/parameter:self", builtin.as_str())));
        let receiver_ty = receiver.ty.clone();
        let mut lowered = vec![ResolvedArgument {
            parameter: receiver_parameter,
            value: receiver,
            conversion: CheckedConversion {
                kind: CheckedConversionKind::Identity,
                from: receiver_ty.clone(),
                to: receiver_ty,
            },
        }];
        for index in 0..arguments.len() {
            if matches!(arguments[index].unlocated(), Expr::NamedArg(_, _)) {
                return self
                    .unsupported(node_id, "named arguments on language-provided method call");
            }
            let argument_role = expr_sibling_role(&format!("{role}.argument"), arguments, index);
            let value = self.lower_expr(&arguments[index], &argument_role)?;
            let value_ty = value.ty.clone();
            lowered.push(ResolvedArgument {
                parameter: super::ResolvedParameterId(NodeId(format!(
                    "{}/parameter:{index}",
                    builtin.as_str()
                ))),
                value,
                conversion: CheckedConversion {
                    kind: CheckedConversionKind::Identity,
                    from: value_ty.clone(),
                    to: value_ty,
                },
            });
        }
        Ok(Some(ResolvedCall {
            callee: ResolvedCallee::Builtin(builtin),
            result: self.expression_type(node_id)?,
            type_arguments: Vec::new(),
            arguments: lowered,
            permission: Some(method.permission),
            effects: Vec::new(),
            session: Vec::new(),
        }))
    }

    fn lower_expr_list(
        &mut self,
        values: &[Expr],
        role: &str,
    ) -> Result<Vec<ResolvedExpr>, Vec<ResolvedBodyError>> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| self.lower_expr(value, &expr_sibling_role(role, values, index)))
            .collect()
    }

    fn lower_binding_pattern(
        &mut self,
        pattern: &Pattern,
        role: &str,
        ty: ResolvedTypeId,
        mutable: bool,
    ) -> Result<ResolvedPattern, Vec<ResolvedBodyError>> {
        let node_id = self.pattern_id(pattern, role)?;
        let origin = self.origin(&node_id)?;
        let kind = match &pattern.kind {
            PatternKind::Wildcard => ResolvedPatternKind::Wildcard,
            PatternKind::Variable(name) => {
                if let Some(constructor) =
                    self.lower_constructor_pattern(&node_id, name, &[], role, &ty, mutable)?
                {
                    return Ok(ResolvedPattern {
                        node_id,
                        origin,
                        ty,
                        kind: constructor,
                    });
                }
                let local_id = ResolvedLocalId(NodeId(format!("{}/local", node_id.0)));
                self.insert_local(
                    name.clone(),
                    ResolvedLocal {
                        id: local_id.clone(),
                        display_name: name.clone(),
                        ty: ty.clone(),
                        mutable,
                        origin: origin.clone(),
                    },
                    &node_id,
                )?;
                ResolvedPatternKind::Binding {
                    local: local_id,
                    by_reference: None,
                }
            }
            PatternKind::Literal(literal) => {
                ResolvedPatternKind::Literal(self.lower_literal(&node_id, literal)?)
            }
            PatternKind::Tuple(patterns) => {
                let element_types = match self.types.get(&ty) {
                    Some(ResolvedType::Tuple(elements)) if elements.len() == patterns.len() => {
                        elements.clone()
                    }
                    _ => {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "tuple pattern shape disagrees with canonical scrutinee type",
                        )])
                    }
                };
                ResolvedPatternKind::Tuple(self.lower_pattern_list(
                    patterns,
                    role,
                    &element_types,
                    mutable,
                )?)
            }
            PatternKind::Array(patterns) => {
                let element = match self.types.get(&ty) {
                    Some(ResolvedType::Array { element, length }) if *length == patterns.len() => {
                        element.clone()
                    }
                    Some(ResolvedType::Nominal { item, arguments })
                        if item.as_str() == "builtin:type:List" && arguments.len() == 1 =>
                    {
                        arguments[0].clone()
                    }
                    _ => {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "array pattern shape disagrees with canonical scrutinee type",
                        )])
                    }
                };
                let element_types = vec![element; patterns.len()];
                ResolvedPatternKind::Array(self.lower_pattern_list(
                    patterns,
                    role,
                    &element_types,
                    mutable,
                )?)
            }
            PatternKind::Slice(patterns, rest) => {
                let element = match self.types.get(&ty) {
                    Some(ResolvedType::Array { element, .. })
                    | Some(ResolvedType::Slice(element)) => element.clone(),
                    Some(ResolvedType::Nominal { item, arguments })
                        if item.as_str() == "builtin:type:List" && arguments.len() == 1 =>
                    {
                        arguments[0].clone()
                    }
                    _ => return self.unsupported(&node_id, "slice pattern on non-sequence type"),
                };
                let element_types = vec![element; patterns.len()];
                let prefix = self.lower_pattern_list(patterns, role, &element_types, mutable)?;
                let rest = rest
                    .as_ref()
                    .map(|rest| {
                        self.lower_binding_pattern(
                            rest,
                            &format!("{role}.rest"),
                            ty.clone(),
                            mutable,
                        )
                        .map(Box::new)
                    })
                    .transpose()?;
                ResolvedPatternKind::Slice { prefix, rest }
            }
            PatternKind::Constructor(name, fields) => self
                .lower_constructor_pattern(&node_id, name, fields, role, &ty, mutable)?
                .ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!(
                            "constructor '{name}' is absent from canonical scrutinee type '{}'",
                            ty.as_str()
                        ),
                    )]
                })?,
        };
        Ok(ResolvedPattern {
            node_id,
            origin,
            ty,
            kind,
        })
    }

    fn lower_constructor_pattern(
        &mut self,
        node_id: &NodeId,
        name: &str,
        fields: &[(String, Pattern)],
        role: &str,
        ty: &ResolvedTypeId,
        mutable: bool,
    ) -> Result<Option<ResolvedPatternKind>, Vec<ResolvedBodyError>> {
        if let Some(ResolvedType::Newtype { item, inner }) = self.types.get(ty) {
            let owner = NodeId(item.as_str().to_string());
            let matches_name = self.type_defs.get(&owner).is_some_and(|definition| {
                definition.kind == ResolvedTypeKind::Newtype
                    && (definition.qualified_name == name
                        || definition
                            .qualified_name
                            .rsplit_once("::")
                            .is_some_and(|(_, short)| short == name))
            });
            if matches_name {
                let declared = vec![(
                    "_0".to_string(),
                    NodeId(format!("{}/payload:0", owner.0)),
                    inner.clone(),
                )];
                let lowered =
                    self.lower_constructor_fields(node_id, fields, role, &declared, mutable)?;
                return Ok(Some(ResolvedPatternKind::Constructor {
                    variant: owner,
                    fields: lowered,
                }));
            }
        }
        if let Some(ResolvedType::Nominal { item, .. }) = self.types.get(ty) {
            let owner = NodeId(item.as_str().to_string());
            if let Some(definition) = self.type_defs.get(&owner) {
                if let Some((variant_id, declared)) =
                    self.instantiated_variant_fields(node_id, ty, definition, name)?
                {
                    let lowered =
                        self.lower_constructor_fields(node_id, fields, role, &declared, mutable)?;
                    return Ok(Some(ResolvedPatternKind::Constructor {
                        variant: variant_id,
                        fields: lowered,
                    }));
                }
            }
        }

        let builtin = match (name, self.types.get(ty)) {
            ("Some", Some(ResolvedType::Option(inner))) => Some((
                "builtin:variant:Option::Some",
                vec![(
                    "_0".to_string(),
                    NodeId("builtin:variant:Option::Some/payload:0".into()),
                    inner.clone(),
                )],
            )),
            ("None", Some(ResolvedType::Option(_))) => {
                Some(("builtin:variant:Option::None", Vec::new()))
            }
            ("Ok", Some(ResolvedType::Result { ok, .. })) => Some((
                "builtin:variant:Result::Ok",
                vec![(
                    "_0".to_string(),
                    NodeId("builtin:variant:Result::Ok/payload:0".into()),
                    ok.clone(),
                )],
            )),
            ("Err", Some(ResolvedType::Result { error, .. })) => Some((
                "builtin:variant:Result::Err",
                vec![(
                    "_0".to_string(),
                    NodeId("builtin:variant:Result::Err/payload:0".into()),
                    error.clone(),
                )],
            )),
            ("Some", Some(ResolvedType::Nominal { item, arguments }))
                if item.as_str() == "builtin:type:Option" && arguments.len() == 1 =>
            {
                Some((
                    "builtin:variant:Option::Some",
                    vec![(
                        "_0".to_string(),
                        NodeId("builtin:variant:Option::Some/payload:0".into()),
                        arguments[0].clone(),
                    )],
                ))
            }
            ("None", Some(ResolvedType::Nominal { item, arguments }))
                if item.as_str() == "builtin:type:Option" && arguments.len() == 1 =>
            {
                Some(("builtin:variant:Option::None", Vec::new()))
            }
            ("Ok", Some(ResolvedType::Nominal { item, arguments }))
                if item.as_str() == "builtin:type:Result" && arguments.len() == 2 =>
            {
                Some((
                    "builtin:variant:Result::Ok",
                    vec![(
                        "_0".to_string(),
                        NodeId("builtin:variant:Result::Ok/payload:0".into()),
                        arguments[0].clone(),
                    )],
                ))
            }
            ("Err", Some(ResolvedType::Nominal { item, arguments }))
                if item.as_str() == "builtin:type:Result" && arguments.len() == 2 =>
            {
                Some((
                    "builtin:variant:Result::Err",
                    vec![(
                        "_0".to_string(),
                        NodeId("builtin:variant:Result::Err/payload:0".into()),
                        arguments[1].clone(),
                    )],
                ))
            }
            _ => None,
        };
        let Some((variant, declared)) = builtin else {
            return Ok(None);
        };
        let lowered = self.lower_constructor_fields(node_id, fields, role, &declared, mutable)?;
        Ok(Some(ResolvedPatternKind::Constructor {
            variant: NodeId(variant.into()),
            fields: lowered,
        }))
    }

    fn lower_constructor_fields(
        &mut self,
        node_id: &NodeId,
        fields: &[(String, Pattern)],
        role: &str,
        declared: &[(String, NodeId, ResolvedTypeId)],
        mutable: bool,
    ) -> Result<Vec<(NodeId, ResolvedPattern)>, Vec<ResolvedBodyError>> {
        if fields.len() != declared.len() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "constructor payload count {} does not match canonical declaration count {}",
                    fields.len(),
                    declared.len()
                ),
            )]);
        }
        let mut supplied = BTreeMap::new();
        for (name, pattern) in fields {
            if supplied.insert(name.as_str(), pattern).is_some() {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("constructor field '{name}' is supplied more than once"),
                )]);
            }
        }
        let mut lowered = Vec::with_capacity(declared.len());
        for (name, field, field_ty) in declared {
            let pattern = supplied.remove(name.as_str()).ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("constructor field '{name}' has no checked pattern"),
                )]
            })?;
            let pattern = self.lower_binding_pattern(
                pattern,
                &format!("{role}.field.{}", stable_id_fragment(name)),
                field_ty.clone(),
                mutable,
            )?;
            lowered.push((field.clone(), pattern));
        }
        if let Some(extra) = supplied.keys().next() {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("constructor field '{extra}' has no declaration"),
            )]);
        }
        Ok(lowered)
    }

    fn lower_pattern_list(
        &mut self,
        patterns: &[Pattern],
        role: &str,
        types: &[ResolvedTypeId],
        mutable: bool,
    ) -> Result<Vec<ResolvedPattern>, Vec<ResolvedBodyError>> {
        patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                self.lower_binding_pattern(
                    pattern,
                    &pattern_sibling_role(&format!("{role}.element"), patterns, index),
                    types[index].clone(),
                    mutable,
                )
            })
            .collect()
    }

    fn lower_place(
        &mut self,
        expr: &Expr,
        role: &str,
    ) -> Result<ResolvedPlace, Vec<ResolvedBodyError>> {
        let node_id = self.expr_id(expr, role)?;
        match expr.unlocated() {
            Expr::Ident(name) => {
                self.lookup_local(name)
                    .map(ResolvedPlace::root)
                    .ok_or_else(|| {
                        vec![ResolvedBodyError::new(
                            node_id,
                            format!("place base '{name}' has no resolved local identity"),
                        )]
                    })
            }
            Expr::TupleIndex(base, index) => {
                let mut place = self.lower_place(base, &format!("{role}.inner"))?;
                let ty = self.expression_type(&node_id)?;
                place
                    .projections
                    .push(ResolvedProjection::Tuple { index: *index, ty });
                Ok(place)
            }
            Expr::Field(base, name) => {
                let mut place = self.lower_place(base, &format!("{role}.inner"))?;
                let base_ty = self.place_type(&node_id, &place)?;
                let field = self.resolve_field(&node_id, &base_ty, name)?;
                let ty = self.expression_type(&node_id)?;
                place.projections.push(ResolvedProjection::Field {
                    field,
                    name: name.clone(),
                    ty,
                });
                Ok(place)
            }
            Expr::Index(base, index) => {
                let mut place = self.lower_place(base, &format!("{role}.left"))?;
                let ty = self.expression_type(&node_id)?;
                let index = match index.unlocated() {
                    Expr::Literal(Lit::Int(value)) => super::ResolvedIndex::Constant(*value),
                    _ => {
                        let input = self.lower_expr(index, &format!("{role}.right"))?;
                        let input_id = input.node_id.clone();
                        if self.place_inputs.insert(input_id.clone(), input).is_some() {
                            return Err(vec![ResolvedBodyError::new(
                                input_id,
                                "dynamic place input identity collision",
                            )]);
                        }
                        super::ResolvedIndex::Dynamic(input_id)
                    }
                };
                place
                    .projections
                    .push(ResolvedProjection::Index { index, ty });
                Ok(place)
            }
            Expr::Unary(UnOp::Deref, base) => {
                let mut place = self.lower_place(base, &format!("{role}.inner"))?;
                let ty = self.expression_type(&node_id)?;
                place.projections.push(ResolvedProjection::Deref { ty });
                Ok(place)
            }
            _ => self.unsupported(&node_id, "projected place lowering"),
        }
    }

    fn lower_drop_places(
        &mut self,
        expression: &Expr,
        role: &str,
    ) -> Result<Vec<ResolvedPlace>, Vec<ResolvedBodyError>> {
        match expression.unlocated() {
            Expr::Tuple(elements) | Expr::List(elements) => {
                let mut places = Vec::new();
                for index in 0..elements.len() {
                    let element_role =
                        expr_sibling_role(&format!("{role}.element"), elements, index);
                    places.extend(self.lower_drop_places(&elements[index], &element_role)?);
                }
                if places.is_empty() {
                    return Err(vec![ResolvedBodyError::new(
                        self.expr_id(expression, role)?,
                        "aggregate drop has no resource places",
                    )]);
                }
                Ok(places)
            }
            _ => Ok(vec![self.lower_place(expression, role)?]),
        }
    }

    fn expression_type(&self, node_id: &NodeId) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        self.node_types.get(node_id).cloned().ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                "place expression has no checker-finalized canonical type",
            )]
        })
    }

    fn resolve_field(
        &self,
        node_id: &NodeId,
        base_ty: &ResolvedTypeId,
        name: &str,
    ) -> Result<NodeId, Vec<ResolvedBodyError>> {
        let nominal = match self.types.get(base_ty) {
            Some(ResolvedType::Nominal { item, .. }) | Some(ResolvedType::Newtype { item, .. }) => {
                item
            }
            _ => return self.unsupported(node_id, "field projection on non-nominal type"),
        };
        let owner = NodeId(nominal.as_str().to_string());
        let field_id = if let Some(definition) = self.type_defs.get(&owner) {
            if !matches!(
                definition.kind,
                ResolvedTypeKind::Record | ResolvedTypeKind::Union
            ) {
                return self.unsupported(node_id, "field projection on non-record nominal type");
            }
            definition.field_ids.get(name).cloned().ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("field '{name}' is absent from nominal owner '{}'", owner.0),
                )]
            })?
        } else if crate::core::resolved::builtin_record_schema(&owner.0)
            .is_some_and(|schema| schema.iter().any(|(field, _)| *field == name))
        {
            NodeId(format!("{}/field:{name}", owner.0))
        } else if let Some(actor) = self.actors.get(&owner) {
            actor.field_ids.get(name).cloned().ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("field '{name}' is absent from actor owner '{}'", owner.0),
                )]
            })?
        } else {
            let states = self
                .flows
                .values()
                .flat_map(|flow| flow.states.values())
                .filter(|state| state.node_id == owner)
                .collect::<Vec<_>>();
            let [state] = states.as_slice() else {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("nominal owner '{}' has no unique field catalog", owner.0),
                )]);
            };
            state.field_ids.get(name).cloned().ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!("field '{name}' is absent from state owner '{}'", owner.0),
                )]
            })?
        };
        let builtin = field_id.0.starts_with("builtin:type:");
        if (!builtin && !self.node_meta.contains_key(&field_id))
            || !self.field_types.contains_key(&field_id)
        {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("field '{name}' has no canonical declaration facts"),
            )]);
        }
        Ok(field_id)
    }

    fn instantiate_member_type(
        &self,
        node_id: &NodeId,
        owner_ty: &ResolvedTypeId,
        declaration_ty: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        let mut substitutions = BTreeMap::new();
        if let Some(ResolvedType::Nominal { item, arguments }) = self.types.get(owner_ty) {
            let owner = NodeId(item.as_str().to_string());
            if let Some(definition) = self.type_defs.get(&owner) {
                if definition.generic_parameters.len() != arguments.len() {
                    return Err(vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!(
                            "nominal owner '{}' has {} canonical arguments for {} generic binders",
                            owner.0,
                            arguments.len(),
                            definition.generic_parameters.len()
                        ),
                    )]);
                }
                for ((_, binder), argument) in definition.generic_parameters.iter().zip(arguments) {
                    substitutions.insert(binder.clone(), argument.clone());
                }
            }
        }
        self.substitute_member_type(node_id, declaration_ty, &substitutions)
    }

    fn instantiated_variant_fields(
        &self,
        node_id: &NodeId,
        owner_ty: &ResolvedTypeId,
        definition: &ResolvedTypeDef,
        name: &str,
    ) -> Result<Option<InstantiatedVariant>, Vec<ResolvedBodyError>> {
        if definition.kind != ResolvedTypeKind::Enum {
            return Ok(None);
        }
        let Some(variant_id) = definition.variant_ids.get(name) else {
            return Ok(None);
        };
        let schema = self.variants.get(variant_id).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                variant_id.clone(),
                format!("enum variant '{name}' has no canonical payload schema"),
            )]
        })?;
        let mut fields = Vec::with_capacity(schema.members.len());
        for member in &schema.members {
            fields.push((
                member.name.clone(),
                member.node_id.clone(),
                self.instantiate_member_type(node_id, owner_ty, &member.ty)?,
            ));
        }
        Ok(Some((variant_id.clone(), fields)))
    }

    fn substitute_member_type(
        &self,
        node_id: &NodeId,
        declaration_ty: &ResolvedTypeId,
        substitutions: &BTreeMap<NodeId, ResolvedTypeId>,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        let declaration = self.types.get(declaration_ty).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "member references missing canonical type '{}'",
                    declaration_ty.as_str()
                ),
            )]
        })?;
        let substitute =
            |child: &ResolvedTypeId| self.substitute_member_type(node_id, child, substitutions);
        let resolved = match declaration {
            ResolvedType::GenericParameter(parameter) => {
                return substitutions.get(parameter).cloned().ok_or_else(|| {
                    vec![ResolvedBodyError::new(
                        node_id.clone(),
                        format!(
                            "member generic binder '{}' has no canonical instantiation",
                            parameter.0
                        ),
                    )]
                })
            }
            ResolvedType::Nominal { item, arguments } => ResolvedType::Nominal {
                item: item.clone(),
                arguments: arguments
                    .iter()
                    .map(substitute)
                    .collect::<Result<Vec<_>, _>>()?,
            },
            ResolvedType::Reference {
                lifetime,
                mutable,
                target,
            } => ResolvedType::Reference {
                lifetime: lifetime.clone(),
                mutable: *mutable,
                target: substitute(target)?,
            },
            ResolvedType::Option(inner) => ResolvedType::Option(substitute(inner)?),
            ResolvedType::Result { ok, error } => ResolvedType::Result {
                ok: substitute(ok)?,
                error: substitute(error)?,
            },
            ResolvedType::Tuple(elements) => ResolvedType::Tuple(
                elements
                    .iter()
                    .map(substitute)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            ResolvedType::Function {
                abi,
                parameters,
                result,
            } => ResolvedType::Function {
                abi: *abi,
                parameters: parameters
                    .iter()
                    .map(substitute)
                    .collect::<Result<Vec<_>, _>>()?,
                result: substitute(result)?,
            },
            ResolvedType::CBuffer(inner) => ResolvedType::CBuffer(substitute(inner)?),
            ResolvedType::Ownership { kind, target } => ResolvedType::Ownership {
                kind: *kind,
                target: substitute(target)?,
            },
            ResolvedType::Newtype { item, inner } => ResolvedType::Newtype {
                item: item.clone(),
                inner: substitute(inner)?,
            },
            ResolvedType::Array { element, length } => ResolvedType::Array {
                element: substitute(element)?,
                length: *length,
            },
            ResolvedType::Slice(inner) => ResolvedType::Slice(substitute(inner)?),
            ResolvedType::RawPointer { mutable, target } => ResolvedType::RawPointer {
                mutable: *mutable,
                target: substitute(target)?,
            },
            ResolvedType::CShared(inner) => ResolvedType::CShared(substitute(inner)?),
            ResolvedType::CBorrow { mutable, target } => ResolvedType::CBorrow {
                mutable: *mutable,
                target: substitute(target)?,
            },
            ResolvedType::Primitive(_)
            | ResolvedType::Capability(_)
            | ResolvedType::Nothing
            | ResolvedType::Allocator
            | ResolvedType::Trait { .. }
            | ResolvedType::RawString
            | ResolvedType::DynamicAny { .. } => return Ok(declaration_ty.clone()),
        };
        if &resolved == declaration {
            return Ok(declaration_ty.clone());
        }
        self.types
            .iter()
            .find_map(|(id, candidate)| (candidate == &resolved).then(|| id.clone()))
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "instantiated member type is absent from the canonical type table",
                )]
            })
    }

    fn place_type(
        &self,
        node_id: &NodeId,
        place: &ResolvedPlace,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        self.locals
            .get(&place.base)
            .map(|local| place.projected_type(local).clone())
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "place references an unknown local",
                )]
            })
    }

    fn iterable_element_type(
        &self,
        node_id: &NodeId,
        iterable: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        match self.types.get(iterable) {
            Some(ResolvedType::Array { element, .. }) | Some(ResolvedType::Slice(element)) => {
                Ok(element.clone())
            }
            Some(ResolvedType::Nominal { item, arguments })
                if matches!(
                    item.as_str(),
                    "builtin:type:List" | "builtin:type:Set" | "builtin:type:Range"
                ) && arguments.len() == 1 =>
            {
                Ok(arguments[0].clone())
            }
            _ => self.unsupported(node_id, "for loop over non-canonical iterable type"),
        }
    }

    fn optional_success_type(
        &self,
        node_id: &NodeId,
        ty: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        match self.types.get(ty) {
            Some(ResolvedType::Option(inner)) => Ok(inner.clone()),
            Some(ResolvedType::Result { ok, .. }) => Ok(ok.clone()),
            Some(ResolvedType::Nominal { item, arguments })
                if item.as_str() == "builtin:type:Option" && arguments.len() == 1 =>
            {
                Ok(arguments[0].clone())
            }
            Some(ResolvedType::Nominal { item, arguments })
                if item.as_str() == "builtin:type:Result" && arguments.len() == 2 =>
            {
                Ok(arguments[0].clone())
            }
            _ => self.unsupported(node_id, "optional chain on non-Option/Result type"),
        }
    }

    fn collect_instantiation(
        &self,
        template: &ResolvedTypeId,
        actual: &ResolvedTypeId,
        substitutions: &mut BTreeMap<NodeId, ResolvedTypeId>,
    ) -> bool {
        let Some(template_ty) = self.types.get(template) else {
            return false;
        };
        if let ResolvedType::GenericParameter(parameter) = template_ty {
            return match substitutions.get(parameter) {
                Some(instantiated) => instantiated == actual,
                None => {
                    substitutions.insert(parameter.clone(), actual.clone());
                    true
                }
            };
        }
        if template == actual {
            return true;
        }
        let Some(actual_ty) = self.types.get(actual) else {
            return false;
        };
        match (template_ty, actual_ty) {
            (
                ResolvedType::Nominal {
                    item: left_item,
                    arguments: left,
                },
                ResolvedType::Nominal {
                    item: right_item,
                    arguments: right,
                },
            ) => {
                left_item == right_item
                    && left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| self.collect_instantiation(left, right, substitutions))
            }
            (ResolvedType::Option(left), ResolvedType::Option(right))
            | (ResolvedType::CBuffer(left), ResolvedType::CBuffer(right))
            | (ResolvedType::Slice(left), ResolvedType::Slice(right))
            | (ResolvedType::CShared(left), ResolvedType::CShared(right)) => {
                self.collect_instantiation(left, right, substitutions)
            }
            (
                ResolvedType::Result {
                    ok: left_ok,
                    error: left_error,
                },
                ResolvedType::Result {
                    ok: right_ok,
                    error: right_error,
                },
            ) => {
                self.collect_instantiation(left_ok, right_ok, substitutions)
                    && self.collect_instantiation(left_error, right_error, substitutions)
            }
            (ResolvedType::Tuple(left), ResolvedType::Tuple(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| self.collect_instantiation(left, right, substitutions))
            }
            (
                ResolvedType::Reference {
                    lifetime: left_lifetime,
                    mutable: left_mutable,
                    target: left,
                },
                ResolvedType::Reference {
                    lifetime: right_lifetime,
                    mutable: right_mutable,
                    target: right,
                },
            ) => {
                left_lifetime == right_lifetime
                    && left_mutable == right_mutable
                    && self.collect_instantiation(left, right, substitutions)
            }
            (
                ResolvedType::Function {
                    abi: left_abi,
                    parameters: left_parameters,
                    result: left_result,
                },
                ResolvedType::Function {
                    abi: right_abi,
                    parameters: right_parameters,
                    result: right_result,
                },
            ) => {
                left_abi == right_abi
                    && left_parameters.len() == right_parameters.len()
                    && left_parameters
                        .iter()
                        .zip(right_parameters)
                        .all(|(left, right)| self.collect_instantiation(left, right, substitutions))
                    && self.collect_instantiation(left_result, right_result, substitutions)
            }
            (
                ResolvedType::Ownership {
                    kind: left_kind,
                    target: left,
                },
                ResolvedType::Ownership {
                    kind: right_kind,
                    target: right,
                },
            ) => left_kind == right_kind && self.collect_instantiation(left, right, substitutions),
            (
                ResolvedType::Newtype {
                    item: left_item,
                    inner: left,
                },
                ResolvedType::Newtype {
                    item: right_item,
                    inner: right,
                },
            ) => left_item == right_item && self.collect_instantiation(left, right, substitutions),
            (
                ResolvedType::Array {
                    element: left,
                    length: left_length,
                },
                ResolvedType::Array {
                    element: right,
                    length: right_length,
                },
            ) => {
                left_length == right_length
                    && self.collect_instantiation(left, right, substitutions)
            }
            (
                ResolvedType::RawPointer {
                    mutable: left_mutable,
                    target: left,
                },
                ResolvedType::RawPointer {
                    mutable: right_mutable,
                    target: right,
                },
            )
            | (
                ResolvedType::CBorrow {
                    mutable: left_mutable,
                    target: left,
                },
                ResolvedType::CBorrow {
                    mutable: right_mutable,
                    target: right,
                },
            ) => {
                left_mutable == right_mutable
                    && self.collect_instantiation(left, right, substitutions)
            }
            _ => false,
        }
    }

    fn reference_binding_type(
        &self,
        node_id: &NodeId,
        initializer: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        let matches = self
            .types
            .iter()
            .filter_map(|(id, ty)| match ty {
                ResolvedType::Reference {
                    lifetime: None,
                    mutable: false,
                    target,
                } if target == initializer => Some(id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [reference] = matches.as_slice() else {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "reference binding has no unique checker-finalized canonical reference type",
            )]);
        };
        Ok(reference.clone())
    }

    fn shared_binding_type(
        &self,
        node_id: &NodeId,
        kind: crate::ast::SharedKind,
        initializer: &ResolvedTypeId,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        let expected = match kind {
            crate::ast::SharedKind::Shared => super::OwnershipTypeKind::Shared,
            crate::ast::SharedKind::LocalShared => super::OwnershipTypeKind::LocalShared,
            crate::ast::SharedKind::Weak => super::OwnershipTypeKind::Weak,
            crate::ast::SharedKind::WeakLocal => super::OwnershipTypeKind::WeakLocal,
        };
        let desired_target = match (kind, self.types.get(initializer)) {
            (
                crate::ast::SharedKind::Weak,
                Some(ResolvedType::Ownership {
                    kind: super::OwnershipTypeKind::Shared,
                    target,
                }),
            )
            | (
                crate::ast::SharedKind::WeakLocal,
                Some(ResolvedType::Ownership {
                    kind: super::OwnershipTypeKind::LocalShared,
                    target,
                }),
            ) => target,
            (crate::ast::SharedKind::Weak | crate::ast::SharedKind::WeakLocal, _) => {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "weak binding initializer has no compatible canonical strong ownership type",
                )])
            }
            (crate::ast::SharedKind::Shared | crate::ast::SharedKind::LocalShared, _) => {
                initializer
            }
        };
        let Some(ty) = self.node_types.get(node_id) else {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "shared binding has no checker-finalized canonical ownership type",
            )]);
        };
        if !matches!(
            self.types.get(ty),
            Some(ResolvedType::Ownership {
                kind,
                target,
            }) if *kind == expected && target == desired_target
        ) {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                "shared binding canonical type disagrees with its kind or initializer",
            )]);
        }
        Ok(ty.clone())
    }

    fn lower_literal(
        &self,
        node_id: &NodeId,
        literal: &Lit,
    ) -> Result<ResolvedLiteral, Vec<ResolvedBodyError>> {
        Ok(match literal {
            Lit::Int(value) => ResolvedLiteral::Int(*value),
            Lit::Float(value) => ResolvedLiteral::float(*value),
            Lit::Bool(value) => ResolvedLiteral::Bool(*value),
            Lit::String(value) => ResolvedLiteral::String(value.clone()),
            Lit::Unit => ResolvedLiteral::Unit,
            Lit::FString(_) => return self.unsupported(node_id, "interpolated string literal"),
        })
    }

    fn lower_binary(
        &self,
        node_id: &NodeId,
        op: BinOp,
    ) -> Result<ResolvedBinaryOp, Vec<ResolvedBodyError>> {
        Ok(match op {
            BinOp::Add => ResolvedBinaryOp::Add,
            BinOp::Sub => ResolvedBinaryOp::Subtract,
            BinOp::Mul => ResolvedBinaryOp::Multiply,
            BinOp::Div => ResolvedBinaryOp::Divide,
            BinOp::Mod => ResolvedBinaryOp::Remainder,
            BinOp::Pow => ResolvedBinaryOp::Power,
            BinOp::EqCmp => ResolvedBinaryOp::Equal,
            BinOp::NeCmp => ResolvedBinaryOp::NotEqual,
            BinOp::Lt => ResolvedBinaryOp::Less,
            BinOp::Gt => ResolvedBinaryOp::Greater,
            BinOp::Le => ResolvedBinaryOp::LessEqual,
            BinOp::Ge => ResolvedBinaryOp::GreaterEqual,
            BinOp::And => ResolvedBinaryOp::LogicalAnd,
            BinOp::Or => ResolvedBinaryOp::LogicalOr,
            BinOp::BitAnd => ResolvedBinaryOp::BitAnd,
            BinOp::BitOr => ResolvedBinaryOp::BitOr,
            BinOp::BitXor => ResolvedBinaryOp::BitXor,
            BinOp::Shl => ResolvedBinaryOp::ShiftLeft,
            BinOp::Shr => ResolvedBinaryOp::ShiftRight,
            BinOp::Assign | BinOp::Range => return self.unsupported(node_id, "binary sugar"),
        })
    }

    fn lower_unary(&self, op: UnOp) -> ResolvedUnaryOp {
        match op {
            UnOp::Neg => ResolvedUnaryOp::Negate,
            UnOp::Not => ResolvedUnaryOp::Not,
            UnOp::Ref => ResolvedUnaryOp::BorrowShared,
            UnOp::RefMut => ResolvedUnaryOp::BorrowMutable,
            UnOp::Deref => ResolvedUnaryOp::Dereference,
        }
    }

    fn identity_conversion(
        &self,
        node_id: &NodeId,
        from: &ResolvedTypeId,
        to: &ResolvedTypeId,
    ) -> Result<CheckedConversion, Vec<ResolvedBodyError>> {
        if from != to {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "explicit checked conversion is required from '{}' ({:?}) to '{}' ({:?})",
                    from.as_str(),
                    self.types.get(from),
                    to.as_str(),
                    self.types.get(to)
                ),
            )]);
        }
        Ok(CheckedConversion {
            kind: CheckedConversionKind::Identity,
            from: from.clone(),
            to: to.clone(),
        })
    }

    fn is_system_fault_refinement(
        &self,
        expected: &ResolvedTypeId,
        observed: &ResolvedTypeId,
    ) -> bool {
        matches!(
            (self.types.get(expected), self.types.get(observed)),
            (
                Some(ResolvedType::Nominal { item: state, arguments }),
                Some(ResolvedType::Nominal { item: builtin, arguments: builtin_arguments }),
            ) if state.as_str().starts_with("state:")
                && state.as_str().ends_with("::Fault")
                && arguments.is_empty()
                && builtin.as_str() == "builtin:type:Fault"
                && builtin_arguments.is_empty()
        )
    }

    fn implicit_conversion(
        &self,
        node_id: &NodeId,
        from: &ResolvedTypeId,
        to: &ResolvedTypeId,
    ) -> Result<CheckedConversion, Vec<ResolvedBodyError>> {
        if from == to {
            return self.identity_conversion(node_id, from, to);
        }
        if let Some((kind, target)) = self.instantiated_type_target(node_id, to)? {
            if &target == from {
                let kind = match kind {
                    ResolvedTypeKind::Alias => CheckedConversionKind::AliasWrap,
                    ResolvedTypeKind::Newtype => CheckedConversionKind::NewtypeWrap,
                    _ => {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "non-transparent type unexpectedly has a canonical target",
                        )])
                    }
                };
                return Ok(CheckedConversion {
                    kind,
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
        if let Some((kind, target)) = self.instantiated_type_target(node_id, from)? {
            if &target == to {
                let kind = match kind {
                    ResolvedTypeKind::Alias => CheckedConversionKind::AliasUnwrap,
                    ResolvedTypeKind::Newtype => CheckedConversionKind::NewtypeUnwrap,
                    _ => {
                        return Err(vec![ResolvedBodyError::new(
                            node_id.clone(),
                            "non-transparent type unexpectedly has a canonical target",
                        )])
                    }
                };
                return Ok(CheckedConversion {
                    kind,
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
        if matches!(self.types.get(from), Some(ResolvedType::Slice(target)) if target == to) {
            return Ok(CheckedConversion {
                kind: CheckedConversionKind::SliceView,
                from: from.clone(),
                to: to.clone(),
            });
        }
        if matches!(
            self.types.get(from),
            Some(ResolvedType::Ownership {
                kind: super::OwnershipTypeKind::Shared
                    | super::OwnershipTypeKind::LocalShared,
                target,
            }) if target == to
        ) {
            return Ok(CheckedConversion {
                kind: CheckedConversionKind::OwnershipRead,
                from: from.clone(),
                to: to.clone(),
            });
        }
        if matches!(
            (self.types.get(from), self.types.get(to)),
            (
                Some(ResolvedType::Reference {
                    mutable: from_mutable,
                    target: from_target,
                    ..
                }),
                Some(ResolvedType::Reference {
                    mutable: to_mutable,
                    target: to_target,
                    ..
                })
            ) if from_mutable == to_mutable && from_target == to_target
        ) {
            return Ok(CheckedConversion {
                kind: CheckedConversionKind::LifetimeRebind,
                from: from.clone(),
                to: to.clone(),
            });
        }
        let primitives = match (self.types.get(from), self.types.get(to)) {
            (Some(ResolvedType::Primitive(from)), Some(ResolvedType::Primitive(to))) => {
                (*from, *to)
            }
            _ => {
                return Err(vec![ResolvedBodyError::new(
                    node_id.clone(),
                    format!(
                        "checked implicit conversion is required from '{}' ({:?}) to '{}' ({:?})",
                        from.as_str(),
                        self.types.get(from),
                        to.as_str(),
                        self.types.get(to)
                    ),
                )])
            }
        };
        use super::PrimitiveType;
        if !matches!(
            primitives,
            (PrimitiveType::I32, PrimitiveType::I64)
                | (PrimitiveType::I32, PrimitiveType::F64)
                | (PrimitiveType::I64, PrimitiveType::F64)
        ) {
            return Err(vec![ResolvedBodyError::new(
                node_id.clone(),
                format!(
                    "types '{}' and '{}' have no admitted implicit conversion",
                    from.as_str(),
                    to.as_str()
                ),
            )]);
        }
        Ok(CheckedConversion {
            kind: CheckedConversionKind::NumericWiden,
            from: from.clone(),
            to: to.clone(),
        })
    }

    fn instantiated_type_target(
        &self,
        node_id: &NodeId,
        nominal: &ResolvedTypeId,
    ) -> Result<Option<(ResolvedTypeKind, ResolvedTypeId)>, Vec<ResolvedBodyError>> {
        let item = match self.types.get(nominal) {
            Some(ResolvedType::Nominal { item, .. }) => item,
            Some(ResolvedType::Newtype { inner, .. }) => {
                return Ok(Some((ResolvedTypeKind::Newtype, inner.clone())))
            }
            _ => return Ok(None),
        };
        let owner = NodeId(item.as_str().to_string());
        let Some(definition) = self.type_defs.get(&owner) else {
            return Ok(None);
        };
        if !matches!(
            definition.kind,
            ResolvedTypeKind::Alias | ResolvedTypeKind::Newtype
        ) {
            return Ok(None);
        }
        let target = self.type_targets.get(&owner).ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id.clone(),
                format!("type '{}' has no canonical target", owner.0),
            )]
        })?;
        let target = self.instantiate_member_type(node_id, nominal, target)?;
        Ok(Some((definition.kind, target)))
    }

    fn apply_implicit_conversion(
        &self,
        parent: &NodeId,
        value: ResolvedExpr,
        target: &ResolvedTypeId,
    ) -> Result<ResolvedExpr, Vec<ResolvedBodyError>> {
        let conversion = self.implicit_conversion(parent, &value.ty, target)?;
        if conversion.kind == CheckedConversionKind::Identity {
            return Ok(value);
        }
        Ok(ResolvedExpr {
            node_id: NodeId(format!("{}/implicit-conversion", value.node_id.0)),
            origin: Origin::Desugared {
                parent: value.node_id.clone(),
                rule: "resolved_body.implicit_conversion".into(),
                span: value.origin.user_span(),
            },
            ty: target.clone(),
            effects: value.effects.clone(),
            backend_requirements: value.backend_requirements.clone(),
            kind: ResolvedExprKind::Cast {
                value: Box::new(value),
                conversion,
            },
        })
    }

    fn checked_explicit_conversion(
        &self,
        node_id: &NodeId,
        from: &ResolvedTypeId,
        to: &ResolvedTypeId,
    ) -> Result<CheckedConversion, Vec<ResolvedBodyError>> {
        if from == to {
            return self.identity_conversion(node_id, from, to);
        }
        let numeric = |ty: &ResolvedTypeId| match self.types.get(ty) {
            Some(ResolvedType::Primitive(primitive)) => numeric_width(*primitive),
            _ => None,
        };
        let (from_width, to_width) = match (numeric(from), numeric(to)) {
            (Some(from), Some(to)) => (from, to),
            _ => {
                return self.unsupported(
                    node_id,
                    &format!(
                        "checked conversion from '{}' to '{}'",
                        from.as_str(),
                        to.as_str()
                    ),
                )
            }
        };
        Ok(CheckedConversion {
            kind: if to_width > from_width {
                CheckedConversionKind::NumericWiden
            } else {
                CheckedConversionKind::NumericNarrowChecked
            },
            from: from.clone(),
            to: to.clone(),
        })
    }

    fn insert_local(
        &mut self,
        name: String,
        local: ResolvedLocal,
        owner: &NodeId,
    ) -> Result<(), Vec<ResolvedBodyError>> {
        let local_id = local.id.clone();
        let scope = self.scopes.last_mut().expect("body always has a scope");
        if scope.insert(name.clone(), local_id.clone()).is_some() {
            return Err(vec![ResolvedBodyError::new(
                owner.clone(),
                format!("duplicate local binding '{name}' in one lexical scope"),
            )]);
        }
        if self.locals.insert(local_id.clone(), local).is_some() {
            return Err(vec![ResolvedBodyError::new(
                owner.clone(),
                "stable local identity collision",
            )]);
        }
        if let Some(context) = self.lambda_contexts.last_mut() {
            context.owned.insert(local_id);
        }
        Ok(())
    }

    fn lookup_local(&mut self, name: &str) -> Option<ResolvedLocalId> {
        let local = self
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())?;
        if self.capture_candidates.contains(&local) {
            self.callable_captures.insert(local.clone());
        }
        for context in self.lambda_contexts.iter_mut().rev() {
            if context.owned.contains(&local) {
                break;
            }
            context.captures.insert(local.clone());
        }
        Some(local)
    }

    fn expr_id(&self, expr: &Expr, role: &str) -> Result<NodeId, Vec<ResolvedBodyError>> {
        let meta = expr.meta();
        self.catalogued_id(
            expr_kind(expr),
            role,
            meta.and_then(|meta| usable_span(meta.span)),
            meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User),
        )
    }

    fn stmt_id(&self, stmt: &Stmt, role: &str) -> Result<NodeId, Vec<ResolvedBodyError>> {
        let meta = stmt.meta();
        self.catalogued_id(
            stmt_kind(stmt),
            role,
            stmt_anchor(stmt, self.fallback).map(|(span, _)| span),
            meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User),
        )
    }

    fn pattern_id(&self, pattern: &Pattern, role: &str) -> Result<NodeId, Vec<ResolvedBodyError>> {
        self.catalogued_id(
            pattern_kind(pattern),
            role,
            usable_span(pattern.meta.span),
            pattern.meta.origin,
        )
    }

    fn annotation_type(
        &self,
        annotation: &crate::ast::Type,
        role: &str,
    ) -> Result<ResolvedTypeId, Vec<ResolvedBodyError>> {
        let meta = annotation.meta();
        let node_id = self.catalogued_id(
            type_kind(annotation),
            role,
            meta.and_then(|meta| usable_span(meta.span)),
            meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User),
        )?;
        self.type_operands.get(&node_id).cloned().ok_or_else(|| {
            vec![ResolvedBodyError::new(
                node_id,
                "explicit annotation has no checker-canonical type",
            )]
        })
    }

    fn catalogued_id(
        &self,
        kind: &str,
        role: &str,
        span: Option<Span>,
        origin: AstOrigin,
    ) -> Result<NodeId, Vec<ResolvedBodyError>> {
        let mut diagnostics = Vec::<Diagnostic>::new();
        let id = self
            .ids
            .anonymous(&self.owner, kind, role, span, origin, &mut diagnostics);
        if diagnostics.is_empty() {
            Ok(id)
        } else {
            Err(diagnostics
                .into_iter()
                .map(|diagnostic| ResolvedBodyError::new(id.clone(), diagnostic.message))
                .collect())
        }
    }

    fn block_identity(&self, role: &str, block: &[Stmt]) -> (NodeId, Origin) {
        let mut diagnostics = Vec::new();
        let node_id = self.ids.anonymous(
            &self.owner,
            "body.block",
            role,
            None,
            AstOrigin::Desugared(BLOCK_NORMALIZATION_RULE),
            &mut diagnostics,
        );
        debug_assert!(diagnostics.is_empty());
        let span = block
            .first()
            .and_then(|stmt| stmt_anchor(stmt, self.fallback).map(|(span, _)| span))
            .unwrap_or(self.fallback);
        (
            node_id,
            Origin::Desugared {
                parent: self.owner.clone(),
                rule: BLOCK_NORMALIZATION_RULE.into(),
                span,
            },
        )
    }

    fn origin(&self, node_id: &NodeId) -> Result<Origin, Vec<ResolvedBodyError>> {
        self.node_meta
            .get(node_id)
            .map(|meta| meta.origin.clone())
            .ok_or_else(|| {
                vec![ResolvedBodyError::new(
                    node_id.clone(),
                    "stable node identity is absent from NodeMeta",
                )]
            })
    }

    fn unsupported<T>(
        &self,
        node_id: &NodeId,
        construct: &str,
    ) -> Result<T, Vec<ResolvedBodyError>> {
        Err(vec![ResolvedBodyError::new(
            node_id.clone(),
            format!("typed body lowering does not yet support {construct}"),
        )])
    }
}

fn parse_transition_owner(owner: &NodeId) -> Option<(&str, &str, &str)> {
    let raw = owner.0.strip_prefix("transition:")?;
    let (prefix, source) = raw.rsplit_once("::")?;
    let (flow, event) = prefix.rsplit_once("::")?;
    Some((flow, event, source))
}

fn usable_span(span: Span) -> Option<Span> {
    (span.start_line > 0 && span.start_col > 0).then_some(span)
}

fn numeric_width(primitive: super::PrimitiveType) -> Option<u16> {
    use super::PrimitiveType;
    Some(match primitive {
        PrimitiveType::I8 | PrimitiveType::U8 => 8,
        PrimitiveType::I16 | PrimitiveType::U16 => 16,
        PrimitiveType::I32 | PrimitiveType::U32 | PrimitiveType::F32 => 32,
        PrimitiveType::I64
        | PrimitiveType::U64
        | PrimitiveType::Isize
        | PrimitiveType::Usize
        | PrimitiveType::F64 => 64,
        PrimitiveType::I128 | PrimitiveType::U128 => 128,
        PrimitiveType::Bool | PrimitiveType::Char | PrimitiveType::String | PrimitiveType::Unit => {
            return None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{File, Item};

    fn parse(source: &str) -> File {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse")
    }

    fn function<'a>(file: &'a File, name: &str) -> &'a FuncDef {
        file.items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == name => Some(function),
                _ => None,
            })
            .expect("function")
    }

    #[test]
    fn lowers_basic_checked_function_with_stable_locals() {
        let file = parse(
            "func add(left: i32, right: i32) -> i32 {\n  let mut total = left + right;\n  total = total + 1;\n  total\n}",
        );
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("add").expect("resolved function");
        let signature = program
            .resolved_signature(&resolved.node_id)
            .expect("signature");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "add"),
            signature,
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower body");

        assert_eq!(body.locals.len(), 3);
        assert_eq!(body.root.statements.len(), 2);
        assert!(body.root.result.is_some());
        assert!(body.validate(program.resolved_types()).is_ok());
        assert!(body
            .locals
            .keys()
            .all(|local| local.0 .0.contains("/local")));
    }

    #[test]
    fn missing_expression_type_fails_closed() {
        let file = parse("func identity(value: i32) -> i32 { value }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("identity").expect("resolved function");
        let signature = program
            .resolved_signature(&resolved.node_id)
            .expect("signature");
        let empty = BTreeMap::new();
        let errors = lower_function_body(FunctionBodyInput {
            function: function(&file, "identity"),
            signature,
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: &empty,
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect_err("missing type must fail");
        assert!(errors
            .iter()
            .any(|error| error.message.contains("checker-finalized canonical type")));
    }

    #[test]
    fn dynamic_index_is_retained_as_a_typed_place_input() {
        let file = parse("func read(values: List<i32>, index: i32) -> i32 {\n  values[index]\n}");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("read").expect("resolved function");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "read"),
            signature: program
                .resolved_signature(&resolved.node_id)
                .expect("signature"),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower body");

        assert_eq!(body.place_inputs.len(), 1);
        let result = body.root.result.as_ref().expect("tail result");
        let ResolvedExprKind::Load(place) = &result.kind else {
            panic!("index expression must lower to a place load");
        };
        assert!(matches!(
            place.projections.as_slice(),
            [ResolvedProjection::Index {
                index: crate::core::ResolvedIndex::Dynamic(_),
                ..
            }]
        ));
        body.validate(program.resolved_types())
            .expect("place input is a body node");
    }

    #[test]
    fn indexed_assignment_uses_projected_target_type() {
        let file = parse(
            "func replace(mut values: List<i32>, index: i32) -> i32 {\n  values[index] = 7;\n  values[index]\n}",
        );
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("replace").expect("resolved function");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "replace"),
            signature: program
                .resolved_signature(&resolved.node_id)
                .expect("signature"),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower body");

        let ResolvedStmtKind::Assign {
            target, conversion, ..
        } = &body.root.statements[0].kind
        else {
            panic!("first statement must be assignment");
        };
        assert_eq!(conversion.to, target.projections[0].ty().clone());
        assert_ne!(conversion.to, body.locals[&target.base].ty);
        body.validate(program.resolved_types())
            .expect("indexed assignment validates");
    }

    #[test]
    fn function_call_is_closed_and_named_arguments_use_parameter_order() {
        let file = parse(
            "func subtract(left: i32, right: i32) -> i32 { left - right }\nfunc main() -> i32 { subtract(right = 2, left = 7) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("main").expect("resolved main");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "main"),
            signature: program
                .resolved_signature(&resolved.node_id)
                .expect("signature"),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower body");

        let result = body.root.result.as_ref().expect("tail result");
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("tail must be a resolved call");
        };
        assert_eq!(
            call.arguments
                .iter()
                .map(|argument| argument.parameter.clone())
                .collect::<Vec<_>>(),
            program
                .resolved_signature(&NodeId("function:subtract".into()))
                .expect("callee signature")
                .parameters
                .iter()
                .map(|parameter| parameter.id.clone())
                .collect::<Vec<_>>()
        );
        let values = call
            .arguments
            .iter()
            .map(|argument| match argument.value.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => value,
                _ => panic!("argument must be literal"),
            })
            .collect::<Vec<_>>();
        assert_eq!(values, vec![7, 2]);
        assert!(matches!(
            call.callee,
            ResolvedCallee::Function(ref node) if node == &NodeId("function:subtract".into())
        ));
    }

    #[test]
    fn turbofish_call_retains_canonical_instantiation() {
        let file = parse(
            "func identity<T>(value: T) -> T { value }\nfunc main() -> i32 { identity::<i32>(7) }\nfunc inferred() -> i32 { identity(8) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower turbofish");
        let result = bodies[&NodeId("function:main".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("generic call expected");
        };
        assert_eq!(call.type_arguments.len(), 1);
        assert!(matches!(
            program.resolved_types().get(&call.type_arguments[0]),
            Some(ResolvedType::Primitive(crate::core::ir::PrimitiveType::I32))
        ));
        assert_eq!(
            call.arguments[0].conversion.from,
            call.arguments[0].conversion.to
        );
        assert!(matches!(
            call.callee,
            ResolvedCallee::Function(ref node) if node == &NodeId("function:identity".into())
        ));
        let inferred = bodies[&NodeId("function:inferred".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        let ResolvedExprKind::Call(inferred) = &inferred.kind else {
            panic!("inferred generic call expected");
        };
        assert_eq!(inferred.type_arguments, call.type_arguments);
    }

    #[test]
    fn omitted_default_argument_references_typed_declaration_body() {
        let file = parse(
            "func add(left: i32, right: i32 = 2) -> i32 { left + right }\nfunc main() -> i32 { add(left = 5) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower defaults");
        let add = &bodies[&NodeId("function:add".into())];
        let signature = program
            .resolved_signature(&NodeId("function:add".into()))
            .unwrap();
        let default_parameter = &signature.parameters[1].id;
        let default = &add.default_values[default_parameter];
        assert!(matches!(
            default.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(2))
        ));

        let result = bodies[&NodeId("function:main".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("defaulted call expected");
        };
        assert_eq!(call.arguments.len(), 2);
        assert!(matches!(
            call.arguments[1].value.kind,
            ResolvedExprKind::DefaultArgument {
                ref callable,
                ref parameter,
            } if callable == &NodeId("function:add".into())
                && parameter == default_parameter
        ));
        add.validate(program.resolved_types())
            .expect("valid declaration default");
    }

    #[test]
    fn field_place_reuses_the_declaration_identity() {
        let file = parse("type Point { x: i32 }\nfunc read(point: Point) -> i32 { point.x }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("read").expect("resolved read");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "read"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower field load");
        let ResolvedExprKind::Load(place) = &body.root.result.as_ref().unwrap().kind else {
            panic!("field read must be a place load");
        };
        let ResolvedProjection::Field { field, ty, .. } = &place.projections[0] else {
            panic!("field projection expected");
        };
        assert!(field.0.starts_with("type:Point/node:decl.field@"));
        assert!(program.node_meta().contains_key(field));
        assert_eq!(program.resolved_field_type(field), Some(ty));
    }

    #[test]
    fn match_arms_own_lexical_pattern_bindings() {
        let file =
            parse("func choose(value: i32) -> i32 { match value { 0 => 1, other => other } }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("choose").expect("resolved choose");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "choose"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower match");
        let ResolvedExprKind::Match { arms, .. } = &body.root.result.as_ref().unwrap().kind else {
            panic!("match result expected");
        };
        assert_eq!(arms.len(), 2);
        let ResolvedPatternKind::Binding { local, .. } = &arms[1].pattern.kind else {
            panic!("second arm must bind");
        };
        let ResolvedExprKind::Block(block) = &arms[1].body.kind else {
            panic!("arm body block expected");
        };
        let ResolvedExprKind::Load(place) = &block.result.as_ref().unwrap().kind else {
            panic!("arm body must load binding");
        };
        assert_eq!(&place.base, local);
        body.validate(program.resolved_types())
            .expect("valid match body");
    }

    #[test]
    fn tuple_binding_uses_canonical_element_types() {
        let file = parse("func pick(pair: (i32, i64)) -> i64 { let (left, right) = pair; right }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("pick").expect("resolved pick");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "pick"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower tuple binding");
        let ResolvedStmtKind::Bind { pattern, .. } = &body.root.statements[0].kind else {
            panic!("tuple bind expected");
        };
        let ResolvedPatternKind::Tuple(elements) = &pattern.kind else {
            panic!("tuple pattern expected");
        };
        assert_ne!(elements[0].ty, elements[1].ty);
        body.validate(program.resolved_types())
            .expect("valid tuple bind");
    }

    #[test]
    fn enum_patterns_use_variant_and_payload_declaration_identities() {
        let file = parse(
            "type Choice {\nValue(i32)\nPair { left: i32, right: i32 }\nEmpty\n}\nfunc read(choice: Choice) -> i32 { match choice { Value(value) => value, Pair { right: right, left: left } => left + right, Empty => 0 } }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower enum patterns");
        let body = &bodies[&NodeId("function:read".into())];
        let ResolvedExprKind::Match { arms, .. } = &body.root.result.as_ref().unwrap().kind else {
            panic!("match expected");
        };
        let ResolvedPatternKind::Constructor { variant, fields } = &arms[0].pattern.kind else {
            panic!("tuple variant expected");
        };
        assert!(variant.0.starts_with("type:Choice/node:decl.variant@"));
        assert_eq!(fields.len(), 1);
        assert!(program.resolved_field_type(&fields[0].0).is_some());

        let ResolvedPatternKind::Constructor { fields, .. } = &arms[1].pattern.kind else {
            panic!("record variant expected");
        };
        assert_eq!(fields.len(), 2);
        assert!(fields[0].0 .0.contains("decl.field"));
        let ResolvedPatternKind::Binding { local: left, .. } = &fields[0].1.kind else {
            panic!("left binding expected");
        };
        assert_eq!(body.locals[left].display_name, "left");

        assert!(matches!(
            arms[2].pattern.kind,
            ResolvedPatternKind::Constructor { ref fields, .. } if fields.is_empty()
        ));
        body.validate(program.resolved_types())
            .expect("valid enum patterns");
    }

    #[test]
    fn builtin_option_patterns_retain_payload_type() {
        let file = parse(
            "func unwrap(value: Option<i32>) -> i32 { match value { Some(inner) => inner, None => 0 } }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower option pattern");
        let body = &bodies[&NodeId("function:unwrap".into())];
        let ResolvedExprKind::Match { arms, .. } = &body.root.result.as_ref().unwrap().kind else {
            panic!("match expected");
        };
        let ResolvedPatternKind::Constructor { variant, fields } = &arms[0].pattern.kind else {
            panic!("Some expected");
        };
        assert_eq!(variant.0, "builtin:variant:Option::Some");
        let ResolvedPatternKind::Binding { local, .. } = &fields[0].1.kind else {
            panic!("Some payload binding expected");
        };
        assert_eq!(body.locals[local].ty, fields[0].1.ty);
        assert!(matches!(
            arms[1].pattern.kind,
            ResolvedPatternKind::Constructor { ref fields, .. } if fields.is_empty()
        ));
        body.validate(program.resolved_types())
            .expect("valid builtin patterns");
    }

    #[test]
    fn record_fields_are_sorted_by_declaration_identity() {
        let file =
            parse("type Point { x: i32, y: i32 }\nfunc make() -> Point { Point { y: 2, x: 1 } }");
        let program = crate::core::check_program(&file).expect("check");
        let resolved = program.function("make").expect("resolved make");
        let body = lower_function_body(FunctionBodyInput {
            function: function(&file, "make"),
            signature: program.resolved_signature(&resolved.node_id).unwrap(),
            signatures: program.resolved_signatures(),
            functions: program.functions(),
            type_defs: program.type_defs(),
            variants: program.resolved_variants(),
            actors: program.actors(),
            flows: program.flows(),
            traits: program.traits(),
            impls: program.impls(),
            field_types: program.resolved_field_types(),
            type_targets: program.resolved_type_targets(),
            call_sites: program.call_sites(),
            extern_blocks: program.extern_blocks(),
            constants: program.constants(),
            node_types: program.resolved_node_types(),
            type_operands: program.resolved_type_operands(),
            type_arguments: program.resolved_type_arguments(),
            types: program.resolved_types(),
            node_meta: program.node_meta(),
            sources: &file.sources,
        })
        .expect("lower record");
        let ResolvedExprKind::Record { fields, .. } = &body.root.result.as_ref().unwrap().kind
        else {
            panic!("record expected");
        };
        let values = fields
            .iter()
            .map(|field| match field.value.kind {
                ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => value,
                _ => panic!("literal field expected"),
            })
            .collect::<Vec<_>>();
        assert_eq!(values, vec![1, 2]);
        body.validate(program.resolved_types())
            .expect("valid record");
    }

    #[test]
    fn generic_record_members_use_instantiated_canonical_types() {
        let file = parse(
            "type Box<T> { value: T }\nfunc main() -> i32 { let boxed = Box { value: 42 }; boxed.value }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower generic record");
        let body = &bodies[&NodeId("function:main".into())];
        let ResolvedStmtKind::Bind {
            initializer: Some(initializer),
            ..
        } = &body.root.statements[0].kind
        else {
            panic!("generic record binding expected");
        };
        let ResolvedExprKind::Record { fields, .. } = &initializer.kind else {
            panic!("generic record initializer expected");
        };
        let declaration_type = program
            .resolved_field_type(&fields[0].field)
            .expect("declaration field type");
        assert!(matches!(
            program.resolved_types().get(declaration_type),
            Some(ResolvedType::GenericParameter(_))
        ));
        assert!(matches!(
            program.resolved_types().get(&fields[0].conversion.to),
            Some(ResolvedType::Primitive(crate::core::ir::PrimitiveType::I32))
        ));
        assert_ne!(declaration_type, &fields[0].conversion.to);
        body.validate(program.resolved_types())
            .expect("valid instantiated generic record");
    }

    #[test]
    fn generic_enum_payload_patterns_use_instantiated_types() {
        let file = parse(
            "type Wrapper<T> { Wrap(T) }\nfunc unwrap<T>(input: Wrapper<T>) -> T { match input { Wrap(value) => value } }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower generic enum");
        let body = &bodies[&NodeId("function:unwrap".into())];
        let ResolvedExprKind::Match { arms, .. } = &body.root.result.as_ref().unwrap().kind else {
            panic!("generic enum match expected");
        };
        let ResolvedPatternKind::Constructor { fields, .. } = &arms[0].pattern.kind else {
            panic!("generic constructor pattern expected");
        };
        let declaration_type = program
            .resolved_field_type(&fields[0].0)
            .expect("payload declaration type");
        assert!(matches!(
            program.resolved_types().get(declaration_type),
            Some(ResolvedType::GenericParameter(_))
        ));
        assert!(matches!(
            program.resolved_types().get(&fields[0].1.ty),
            Some(ResolvedType::GenericParameter(_))
        ));
        assert_ne!(declaration_type, &fields[0].1.ty);
        body.validate(program.resolved_types())
            .expect("valid instantiated generic pattern");
    }

    #[test]
    fn explicit_numeric_cast_records_checked_conversion() {
        let file = parse(
            "func widen(value: i32) -> i64 { value as i64 }\nfunc narrow(value: i64) -> i32 { value as i32 }",
        );
        let program = crate::core::check_program(&file).expect("check");
        for (name, expected) in [
            ("widen", CheckedConversionKind::NumericWiden),
            ("narrow", CheckedConversionKind::NumericNarrowChecked),
        ] {
            let body = program
                .resolved_body(&NodeId(format!("function:{name}")))
                .unwrap_or_else(|| panic!("resolved {name}"));
            let ResolvedExprKind::Cast { conversion, .. } =
                &body.root.result.as_ref().unwrap().kind
            else {
                panic!("cast expected for {name}");
            };
            assert_eq!(conversion.kind, expected);
        }
    }

    #[test]
    fn program_body_lowering_is_complete_and_transactional() {
        let file = parse(
            "func increment(value: i32) -> i32 { value + 1 }\nfunc main() -> i32 { increment(4) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("all bodies lower");
        // TOOL-RESOLUTION-001: transition signatures share the canonical
        // catalog, while this entry point intentionally lowers functions only.
        assert_eq!(bodies.len(), program.functions().len());
        assert!(bodies.contains_key(&NodeId("function:increment".into())));
        assert!(bodies.contains_key(&NodeId("function:main".into())));

        let builtin_file = parse("func main() { println(1) }");
        let checked = crate::core::check_program(&builtin_file).expect("checker accepts builtin");
        let bodies = lower_checked_function_bodies(&builtin_file, &checked)
            .expect("builtin identity is canonical");
        let main = &bodies[&NodeId("function:main".into())];
        let expression = main.root.result.as_ref().expect("builtin tail expression");
        assert!(matches!(
            expression.kind,
            ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Builtin(_),
                ..
            })
        ));
    }

    #[test]
    fn extern_call_uses_declaration_and_parameter_identities() {
        let file = parse(
            "extern \"C\" { func c_abs(value: i32) -> i32 }\nfunc main() -> i32 { c_abs(-4) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower extern call");
        let result = bodies[&NodeId("function:main".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("extern call expected");
        };
        let ResolvedCallee::Extern(callee) = &call.callee else {
            panic!("extern identity expected");
        };
        assert!(callee.0.contains("/function:c_abs:"));
        assert!(call.arguments[0]
            .parameter
            .0
             .0
            .contains("decl.extern_parameter"));
    }

    #[test]
    fn for_binding_uses_canonical_iterable_element_type() {
        let file = parse(
            "func total(values: List<i32>) -> i32 { let mut sum = 0; for value in values { sum = sum + value; } sum }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower for loop");
        let function_body = &bodies[&NodeId("function:total".into())];
        let ResolvedStmtKind::For { pattern, body, .. } = &function_body.root.statements[1].kind
        else {
            panic!("for statement expected");
        };
        let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
            panic!("for binding expected");
        };
        assert_eq!(function_body.locals[local].ty, pattern.ty);
        assert_eq!(body.statements.len(), 1);
        function_body
            .validate(program.resolved_types())
            .expect("valid for body");
    }

    #[test]
    fn while_let_retains_typed_pattern_scope_and_initializer() {
        let file = parse(
            "func take(value: Option<i32>) -> i32 { let mut result = 0; while let Some(inner) = value { result = inner; break; } result }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower while-let");
        let body = &bodies[&NodeId("function:take".into())];
        let ResolvedStmtKind::WhileLet {
            pattern,
            initializer,
            body: loop_body,
        } = &body.root.statements[1].kind
        else {
            panic!("while-let expected");
        };
        assert_eq!(pattern.ty, initializer.ty);
        let ResolvedPatternKind::Constructor { fields, .. } = &pattern.kind else {
            panic!("Some pattern expected");
        };
        let ResolvedPatternKind::Binding { local, .. } = &fields[0].1.kind else {
            panic!("payload binding expected");
        };
        let ResolvedStmtKind::Assign { value, .. } = &loop_body.statements[0].kind else {
            panic!("loop assignment expected");
        };
        let ResolvedExprKind::Load(place) = &value.kind else {
            panic!("assignment must load while-let binding");
        };
        assert_eq!(&place.base, local);
        body.validate(program.resolved_types())
            .expect("valid while-let");
    }

    #[test]
    fn pinned_scope_retains_value_timeout_binding_and_capability() {
        let file = parse(
            "func anchor(value: i32) -> i32 { let mut result = 0; pinned(value, timeout = 5) |ptr| { result = ptr; } result }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower pinned");
        let body = &bodies[&NodeId("function:anchor".into())];
        let statement = &body.root.statements[1];
        let ResolvedStmtKind::Pinned {
            value,
            timeout,
            binding: Some(binding),
            body: pinned_body,
        } = &statement.kind
        else {
            panic!("pinned statement expected");
        };
        assert_eq!(body.locals[binding].ty, value.ty);
        assert!(timeout.is_some());
        let ResolvedStmtKind::Assign { value, .. } = &pinned_body.statements[0].kind else {
            panic!("pinned body assignment expected");
        };
        let ResolvedExprKind::Load(place) = &value.kind else {
            panic!("pinned binding load expected");
        };
        assert_eq!(&place.base, binding);
        assert!(statement
            .backend_requirements
            .iter()
            .any(|requirement| requirement.capability == "ffi.pinned"));
        body.validate(program.resolved_types())
            .expect("valid pinned scope");
    }

    #[test]
    fn math_block_retains_typed_verification_expressions() {
        let file = parse(
            "func prove(value: i32) -> i32 { math: { value + 1 > value; value >= 0; }; value }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower math");
        let body = &bodies[&NodeId("function:prove".into())];
        let statement = &body.root.statements[0];
        let ResolvedStmtKind::Math(expressions) = &statement.kind else {
            panic!("math statement expected");
        };
        assert_eq!(expressions.len(), 2);
        assert!(expressions
            .iter()
            .all(|expression| program.resolved_types().get(&expression.ty).is_some()));
        assert!(statement
            .backend_requirements
            .iter()
            .any(|requirement| requirement.capability == "verification.math"));
        body.validate(program.resolved_types())
            .expect("valid math block");
    }

    #[test]
    fn comprehension_binding_is_typed_and_scoped_over_value_and_guard() {
        let file = parse(
            "func select(values: List<i32>) -> List<i32> { [value * 2 for value in values if value > 0] }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower comprehension");
        let body = &bodies[&NodeId("function:select".into())];
        let ResolvedExprKind::Comprehension {
            pattern,
            value,
            guard: Some(guard),
            ..
        } = &body.root.result.as_ref().unwrap().kind
        else {
            panic!("comprehension expected");
        };
        let ResolvedPatternKind::Binding { local, .. } = &pattern.kind else {
            panic!("comprehension binding expected");
        };
        let ResolvedExprKind::Binary { left, .. } = &value.kind else {
            panic!("comprehension value expression expected");
        };
        let ResolvedExprKind::Load(value_place) = &left.kind else {
            panic!("value must load binding");
        };
        let ResolvedExprKind::Binary {
            left: guard_left, ..
        } = &guard.kind
        else {
            panic!("guard expression expected");
        };
        let ResolvedExprKind::Load(guard_place) = &guard_left.kind else {
            panic!("guard must load binding");
        };
        assert_eq!(&value_place.base, local);
        assert_eq!(&guard_place.base, local);
        body.validate(program.resolved_types())
            .expect("valid comprehension");
    }

    #[test]
    fn optional_chain_uses_canonical_inner_field_identity() {
        let file = parse(
            "type Point { x: i32 }\nfunc project(value: Option<Point>) -> Option<i32> { value?.x }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower optional chain");
        let body = &bodies[&NodeId("function:project".into())];
        let ResolvedExprKind::OptionalChain {
            receiver,
            field,
            field_type,
        } = &body.root.result.as_ref().unwrap().kind
        else {
            panic!("optional chain expected");
        };
        assert!(matches!(receiver.kind, ResolvedExprKind::Load(_)));
        assert!(field.0.starts_with("type:Point/node:decl.field@"));
        assert_eq!(program.resolved_field_type(field), Some(field_type));
        body.validate(program.resolved_types())
            .expect("valid optional chain");
    }

    #[test]
    fn optional_chain_instantiates_generic_field_type() {
        let file = parse(
            "type Box<T> { value: T }\nfunc project(value: Option<Box<i32>>) -> Option<i32> { value?.value }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower optional chain");
        let body = &bodies[&NodeId("function:project".into())];
        let ResolvedExprKind::OptionalChain {
            field, field_type, ..
        } = &body.root.result.as_ref().unwrap().kind
        else {
            panic!("optional chain expected");
        };
        assert_ne!(program.resolved_field_type(field), Some(field_type));
        assert!(matches!(
            program.resolved_types().get(field_type),
            Some(ResolvedType::Primitive(crate::core::ir::PrimitiveType::I32))
        ));
        body.validate(program.resolved_types())
            .expect("valid generic optional chain");
    }

    #[test]
    fn formatted_string_retains_typed_interpolation_nodes() {
        let file = parse("func render(value: i32) -> string { f\"value={value + 1}\" }");
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower f-string");
        let body = &bodies[&NodeId("function:render".into())];
        let ResolvedExprKind::FString(parts) = &body.root.result.as_ref().unwrap().kind else {
            panic!("formatted string expected");
        };
        assert!(matches!(
            parts.as_slice(),
            [
                crate::core::ir::ResolvedFStringPart::Text(text),
                crate::core::ir::ResolvedFStringPart::Interpolation(ResolvedExpr {
                    kind: ResolvedExprKind::Binary { .. },
                    ..
                })
            ] if text == "value="
        ));
        body.validate(program.resolved_types())
            .expect("valid formatted string");
    }

    #[test]
    fn type_name_and_old_retain_typed_operands_and_requirements() {
        let file = parse(
            "func type_name_of(value: i32) { type_name(value); () }\nfunc inspect() { type_info(i32); () }\nfunc preserve(value: i32) -> i32 { ensures: result == old(value); value }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies =
            lower_checked_function_bodies(&file, &program).expect("lower semantic wrappers");
        let ResolvedStmtKind::Expr(type_name) = &bodies[&NodeId("function:type_name_of".into())]
            .root
            .statements[0]
            .kind
        else {
            panic!("type-name expression expected");
        };
        assert!(matches!(
            type_name.kind,
            ResolvedExprKind::TypeOf(ref value)
                if matches!(value.kind, ResolvedExprKind::Load(_))
        ));
        assert!(type_name
            .backend_requirements
            .iter()
            .any(|requirement| requirement.capability == "reflection.type_name"));

        let ResolvedStmtKind::Expr(type_info) =
            &bodies[&NodeId("function:inspect".into())].root.statements[0].kind
        else {
            panic!("type-info expression expected");
        };
        let ResolvedExprKind::TypeValue(operand) = &type_info.kind else {
            panic!("canonical type value expected");
        };
        assert!(matches!(
            program.resolved_types().get(operand),
            Some(ResolvedType::Primitive(crate::core::ir::PrimitiveType::I32))
        ));
        assert!(type_info
            .backend_requirements
            .iter()
            .any(|requirement| requirement.capability == "reflection.type_info"));

        let preserve = &bodies[&NodeId("function:preserve".into())];
        let ResolvedStmtKind::Contract { condition, .. } = &preserve.root.statements[0].kind else {
            panic!("ensures contract expected");
        };
        let ResolvedExprKind::Binary { left, right, .. } = &condition.kind else {
            panic!("contract comparison expected");
        };
        assert!(matches!(left.kind, ResolvedExprKind::Load(_)));
        assert!(matches!(right.kind, ResolvedExprKind::Old(_)));
        assert!(right
            .backend_requirements
            .iter()
            .any(|requirement| requirement.capability == "contract.old_snapshot"));
        preserve
            .validate(program.resolved_types())
            .expect("valid old expression");
    }

    #[test]
    fn arena_and_comptime_expressions_retain_typed_scopes() {
        let file = parse(
            "func arena_value() -> i32 { let result = arena { let value = 4; value }; result }\nfunc static_value() -> i32 { comptime { 5 + 1 } }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies =
            lower_checked_function_bodies(&file, &program).expect("lower scoped expressions");
        let ResolvedStmtKind::Bind {
            initializer: Some(arena),
            ..
        } = &bodies[&NodeId("function:arena_value".into())]
            .root
            .statements[0]
            .kind
        else {
            panic!("arena binding expected");
        };
        assert!(matches!(
            arena.kind,
            ResolvedExprKind::Scope {
                kind: crate::core::ir::ResolvedScopeKind::Arena,
                ..
            }
        ));
        assert!(arena
            .backend_requirements
            .iter()
            .any(|requirement| requirement.capability == "allocator.arena"));

        let comptime = bodies[&NodeId("function:static_value".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        assert!(matches!(comptime.kind, ResolvedExprKind::Comptime(_)));
        assert!(comptime
            .backend_requirements
            .iter()
            .any(|requirement| requirement.capability == "comptime.evaluate"));
        bodies[&NodeId("function:static_value".into())]
            .validate(program.resolved_types())
            .expect("valid comptime body");
    }

    #[test]
    fn lambda_retains_typed_parameters_body_and_captures() {
        let file = parse(
            "func inspect(offset: i32) -> i32 { let apply = fn(value: i32) -> i32 { value + offset }; 0 }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower lambda");
        let body = &bodies[&NodeId("function:inspect".into())];
        let ResolvedStmtKind::Bind {
            initializer: Some(initializer),
            ..
        } = &body.root.statements[0].kind
        else {
            panic!("lambda binding expected");
        };
        let ResolvedExprKind::Lambda(lambda) = &initializer.kind else {
            panic!("typed lambda expected");
        };
        assert_eq!(lambda.parameters.len(), 1);
        assert_eq!(lambda.captures.len(), 1);
        assert_eq!(body.locals[&lambda.parameters[0]].display_name, "value");
        assert_eq!(body.locals[&lambda.captures[0]].display_name, "offset");
        let ResolvedExprKind::Binary { left, right, .. } =
            &lambda.body.result.as_ref().expect("lambda result").kind
        else {
            panic!("lambda result must retain its typed expression tree");
        };
        assert!(matches!(
            &left.kind,
            ResolvedExprKind::Load(place) if place.base == lambda.parameters[0]
        ));
        assert!(matches!(
            &right.kind,
            ResolvedExprKind::Load(place) if place.base == lambda.captures[0]
        ));
        body.validate(program.resolved_types())
            .expect("valid typed lambda");
    }

    #[test]
    fn local_closure_call_uses_closed_local_and_parameter_identities() {
        let file = parse(
            "func inspect(offset: i32) -> i32 { let apply = fn(value: i32) -> i32 { value + offset }; apply(2) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower closure call");
        let body = &bodies[&NodeId("function:inspect".into())];
        let result = body.root.result.as_ref().expect("closure call result");
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("resolved closure call expected");
        };
        let ResolvedCallee::LocalClosure(local) = &call.callee else {
            panic!("closure call must use its stable local identity");
        };
        assert_eq!(body.locals[local].display_name, "apply");
        assert_eq!(call.arguments.len(), 1);
        assert!(call.arguments[0].parameter.0 .0.starts_with(&local.0 .0));
        body.validate(program.resolved_types())
            .expect("valid local closure call");
    }

    #[test]
    fn transition_call_closes_source_overload_and_parameter_identities() {
        let file = parse(
            "flow Calc { state Zero { v: i32 } state Value { v: i32 } transition add(Zero, amount: i32) -> Value { do { return Value { v: self.v + amount } } } }\nfunc advance(current: Zero) -> Value { Calc::add(current, amount = 5) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let transition_owner = NodeId("transition:Calc::add::Zero".into());
        let signature = program
            .resolved_signature(&transition_owner)
            .expect("canonical transition signature");
        assert_eq!(signature.parameters.len(), 2);
        assert_eq!(
            signature.parameters[0].permission,
            Some(crate::core::Permission::Consume)
        );

        let bodies = lower_checked_function_bodies(&file, &program).expect("lower transition call");
        let body = &bodies[&NodeId("function:advance".into())];
        let ResolvedExprKind::Call(call) = &body.root.result.as_ref().expect("call result").kind
        else {
            panic!("typed transition call expected");
        };
        assert!(matches!(
            &call.callee,
            ResolvedCallee::Transition(transition)
                if transition.flow.0 == "Calc"
                    && transition.event == "add"
                    && transition.source.name == "Zero"
        ));
        assert_eq!(call.permission, Some(crate::core::Permission::Consume));
        assert_eq!(call.arguments.len(), 2);
        assert_eq!(call.arguments[0].parameter, signature.parameters[0].id);
        assert_eq!(call.arguments[1].parameter, signature.parameters[1].id);
        body.validate(program.resolved_types())
            .expect("valid transition call");
    }

    #[test]
    fn flow_state_records_share_canonical_payload_field_facts() {
        let file = parse(
            "flow Calc { state Zero { v: i32 } state Value { v: i32 } transition add(Zero, amount: i32) -> Value { do { return Value { v: self.v + amount } } } }\nfunc main() -> i32 { let current = Zero { v: 10 }; let next = Calc::add(current, 5); next.v }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let state = &program.flow("Calc").expect("flow").states["Zero"];
        let field = state.field_ids.get("v").expect("state field identity");
        let field_type = program
            .resolved_field_type(field)
            .expect("canonical state field type");
        assert!(matches!(
            program.resolved_types().get(field_type),
            Some(ResolvedType::Primitive(crate::core::ir::PrimitiveType::I32))
        ));

        let bodies = lower_checked_function_bodies(&file, &program).expect("lower flow state body");
        let body = &bodies[&NodeId("function:main".into())];
        let ResolvedStmtKind::Bind {
            initializer: Some(current),
            ..
        } = &body.root.statements[0].kind
        else {
            panic!("state construction expected");
        };
        assert!(matches!(
            &current.kind,
            ResolvedExprKind::Record { fields, .. } if fields[0].field == *field
        ));
        let ResolvedExprKind::Load(place) = &body.root.result.as_ref().expect("field load").kind
        else {
            panic!("state payload projection expected");
        };
        let result_field = program.flow("Calc").unwrap().states["Value"]
            .field_ids
            .get("v")
            .unwrap();
        assert!(matches!(
            place.projections.as_slice(),
            [ResolvedProjection::Field { field: projected, ty, .. }]
                if projected == result_field
                    && Some(ty) == program.resolved_field_type(result_field)
        ));
        body.validate(program.resolved_types())
            .expect("valid typed flow state records");
    }

    #[test]
    fn implemented_transition_body_retains_typed_self_and_payload_construction() {
        let file = parse(
            "flow Calc { state Zero { v: i32 } state Value { v: i32 } transition add(Zero, amount: i32) -> Value { do { return Value { v: self.v + amount } } } }\nfunc main() -> i32 { 0 }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_transition_bodies(&file, &program).expect("lower transition");
        let owner = NodeId("transition:Calc::add::Zero".into());
        let body = &bodies[&owner];
        assert!(body
            .locals
            .values()
            .any(|local| local.display_name == "self"));
        assert!(body
            .locals
            .values()
            .any(|local| local.display_name == "amount"));
        let ResolvedStmtKind::Scope { body: do_body, .. } = &body.root.statements[0].kind else {
            panic!("normalized transition do scope expected");
        };
        let ResolvedStmtKind::Return {
            value: Some(value), ..
        } = &do_body.statements[0].kind
        else {
            panic!("transition return expected");
        };
        let ResolvedExprKind::Record { fields, .. } = &value.kind else {
            panic!("typed target-state construction expected");
        };
        assert_eq!(fields.len(), 1);
        assert!(matches!(
            fields[0].value.kind,
            ResolvedExprKind::Binary { .. }
        ));
        body.validate(program.resolved_types())
            .expect("valid typed transition body");
        let callables = lower_checked_callable_bodies(&file, &program)
            .expect("transactional callable lowering");
        assert!(callables.contains_key(&owner));
        assert!(callables.contains_key(&NodeId("function:main".into())));
    }

    #[test]
    fn constants_and_language_constructors_have_closed_identities() {
        let file = parse(
            "const ANSWER: i32 = 42;\nfunc answer() -> i32 { ANSWER }\nfunc some(value: i32) -> Option<i32> { Some(value) }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower identities");
        let answer = bodies[&NodeId("function:answer".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        assert!(matches!(answer.kind, ResolvedExprKind::Constant(_)));
        let some = bodies[&NodeId("function:some".into())]
            .root
            .result
            .as_ref()
            .unwrap();
        assert!(matches!(
            some.kind,
            ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Builtin(_),
                ..
            })
        ));
    }

    #[test]
    fn transparent_types_retain_targets_conversions_and_constructor_identity() {
        let file = parse(
            "type Count = i32\nnewtype UserId = i32\nfunc count() -> Count { 1 }\nfunc implicit_user_id() -> UserId { 2 }\nfunc user_id(value: i32) -> UserId { UserId(value) }",
        );
        let program = crate::core::check_program(&file).expect("check transparent types");
        for definition in ["type:Count", "type:UserId"] {
            let target = program
                .resolved_type_target(&NodeId(definition.into()))
                .unwrap_or_else(|| panic!("missing canonical target for {definition}"));
            assert!(matches!(
                program.resolved_types().get(target),
                Some(ResolvedType::Primitive(crate::core::ir::PrimitiveType::I32))
            ));
        }

        let count = program
            .resolved_body(&NodeId("function:count".into()))
            .expect("count body");
        let count_result = count.root.result.as_deref().expect("count result");
        assert!(matches!(count_result.kind, ResolvedExprKind::Literal(_)));
        assert_eq!(
            &count_result.ty,
            program
                .resolved_type_target(&NodeId("type:Count".into()))
                .expect("Count target")
        );

        let implicit_user_id = program
            .resolved_body(&NodeId("function:implicit_user_id".into()))
            .expect("implicit_user_id body");
        assert!(matches!(
            implicit_user_id
                .root
                .result
                .as_deref()
                .map(|result| &result.kind),
            Some(ResolvedExprKind::Cast {
                conversion: CheckedConversion {
                    kind: CheckedConversionKind::NewtypeWrap,
                    ..
                },
                ..
            })
        ));

        let user_id = program
            .resolved_body(&NodeId("function:user_id".into()))
            .expect("user_id body");
        assert!(matches!(
            user_id.root.result.as_deref().map(|result| &result.kind),
            Some(ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Constructor(definition),
                ..
            })) if definition == &NodeId("type:UserId".into())
        ));
    }

    #[test]
    fn enum_and_newtype_constructors_are_closed_in_calls_and_patterns() {
        let file = parse(
            "newtype UserId = i32\ntype Shape { Circle(f64) }\nfunc main() -> i32 { let shape = Circle(1.0); let id = UserId(7); match id { UserId(value) => if value == 7 { 0 } else { 1 } } }",
        );
        let program = crate::core::check_program(&file).expect("check constructors");
        let body = program
            .resolved_body(&NodeId("function:main".into()))
            .expect("resolved main");
        let ResolvedStmtKind::Bind {
            initializer: Some(shape),
            ..
        } = &body.root.statements[0].kind
        else {
            panic!("shape binding expected");
        };
        assert!(matches!(
            shape.kind,
            ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Constructor(ref variant),
                ..
            }) if variant.0.starts_with("type:Shape/node:decl.variant@")
        ));
        let result = body.root.result.as_deref().expect("match result");
        let ResolvedExprKind::Match { arms, .. } = &result.kind else {
            panic!("newtype match expected");
        };
        assert!(matches!(
            arms[0].pattern.kind,
            ResolvedPatternKind::Constructor { ref variant, .. }
                if variant == &NodeId("type:UserId".into())
        ));
    }

    #[test]
    fn slice_and_typed_json_builtin_retain_explicit_semantics() {
        let file = parse(
            "type Config { value: i32 }\nfunc take<T>(values: List<T>, n: i32) -> List<T> { if n < len(values) { values[0..n] } else { values } }\nfunc decode(text: string) -> Config { from_json::<Config>(text) }",
        );
        let program = crate::core::check_program(&file).expect("check typed operations");
        let take = program
            .resolved_body(&NodeId("function:take".into()))
            .expect("resolved take");
        let ResolvedExprKind::If { then_block, .. } =
            &take.root.result.as_deref().expect("take result").kind
        else {
            panic!("take if expected");
        };
        assert!(matches!(
            then_block.result.as_deref().map(|result| &result.kind),
            Some(ResolvedExprKind::Cast {
                conversion: CheckedConversion {
                    kind: CheckedConversionKind::SliceView,
                    ..
                },
                ..
            })
        ));

        let decode = program
            .resolved_body(&NodeId("function:decode".into()))
            .expect("resolved decode");
        let ResolvedExprKind::Call(call) =
            &decode.root.result.as_deref().expect("decode result").kind
        else {
            panic!("from_json call expected");
        };
        assert!(matches!(
            call.callee,
            ResolvedCallee::Builtin(ref builtin) if builtin.as_str() == "from_json"
        ));
        assert_eq!(
            call.type_arguments.as_slice(),
            std::slice::from_ref(&call.result)
        );
    }

    #[test]
    fn shared_binding_has_explicit_ownership_wrap() {
        let file = parse("func main() { shared value = 42; println(value) }");
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower shared");
        let body = &bodies[&NodeId("function:main".into())];
        let ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(initializer),
        } = &body.root.statements[0].kind
        else {
            panic!("shared binding expected");
        };
        let ResolvedExprKind::Cast { conversion, .. } = &initializer.kind else {
            panic!("ownership wrap expected");
        };
        assert_eq!(conversion.kind, CheckedConversionKind::OwnershipWrap);
        assert_eq!(conversion.to, pattern.ty);
        body.validate(program.resolved_types())
            .expect("valid shared body");
    }

    #[test]
    fn weak_binding_has_explicit_strong_to_weak_conversion() {
        let file = parse(
            "func main() -> i32 { shared strong = 42; weak observer = strong; if observer.upgrade().deref() == 42 { 0 } else { 1 } }",
        );
        let program = crate::core::check_program(&file).expect("check weak binding");
        let body = program
            .resolved_body(&NodeId("function:main".into()))
            .expect("resolved main");
        let ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(initializer),
        } = &body.root.statements[1].kind
        else {
            panic!("weak binding expected");
        };
        let ResolvedExprKind::Cast { conversion, .. } = &initializer.kind else {
            panic!("ownership downgrade expected");
        };
        assert_eq!(conversion.kind, CheckedConversionKind::OwnershipDowngrade);
        assert_eq!(conversion.to, pattern.ty);
    }

    #[test]
    fn reference_binding_and_shared_read_are_explicit() {
        let file = parse(
            "func read(value: shared i32) -> i32 { value }\nfunc main() -> i32 { arena { let ref value = 42; *value } }",
        );
        let program = crate::core::check_program(&file).expect("check references");
        let read = program
            .resolved_body(&NodeId("function:read".into()))
            .expect("resolved shared read");
        assert!(matches!(
            read.root.result.as_deref().map(|result| &result.kind),
            Some(ResolvedExprKind::Cast {
                conversion: CheckedConversion {
                    kind: CheckedConversionKind::OwnershipRead,
                    ..
                },
                ..
            })
        ));

        let main = program
            .resolved_body(&NodeId("function:main".into()))
            .expect("resolved arena reference");
        let ResolvedStmtKind::Scope { body: arena, .. } = &main.root.statements[0].kind else {
            panic!("arena scope expected");
        };
        let ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(initializer),
        } = &arena.statements[0].kind
        else {
            panic!("reference binding expected");
        };
        assert!(matches!(
            pattern.kind,
            ResolvedPatternKind::Binding {
                by_reference: Some(crate::core::Permission::View),
                ..
            }
        ));
        assert!(matches!(
            initializer.kind,
            ResolvedExprKind::Unary {
                op: ResolvedUnaryOp::BorrowShared,
                ..
            }
        ));
    }

    #[test]
    fn delegate_target_is_closed_to_callable_identity() {
        let file = parse(
            "func child(value: i32) { println(value) }\nfunc main() { let value = 1; delegate view(value) to child }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower delegate");
        let body = &bodies[&NodeId("function:main".into())];
        let ResolvedStmtKind::Delegate { target, .. } = &body.root.statements[1].kind else {
            panic!("delegate expected");
        };
        assert!(matches!(
            target,
            crate::core::ir::DelegateTarget::Callable(node)
                if node == &NodeId("function:child".into())
        ));
    }

    #[test]
    fn actor_method_body_has_canonical_self_parameter() {
        let file = parse(
            "actor Counter { count: i32 func value() -> i32 { self.count } }\nfunc main() -> i32 { 0 }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower actor method");
        let owner = NodeId("function:Counter::value".into());
        let body = &bodies[&owner];
        let signature = program
            .resolved_signature(&owner)
            .expect("method signature");
        assert_eq!(signature.parameters[0].name, "self");
        assert!(body.locals.keys().any(|local| local.0 .0.contains("self")));
        let ResolvedExprKind::Load(place) = &body.root.result.as_ref().expect("field result").kind
        else {
            panic!("actor field must lower as a place load");
        };
        let ResolvedProjection::Field { field, ty, .. } = &place.projections[0] else {
            panic!("actor field projection expected");
        };
        assert!(field.0.starts_with("actor:Counter/node:decl.actor_field@"));
        assert_eq!(program.resolved_field_type(field), Some(ty));
    }

    #[test]
    fn actor_method_call_retains_typed_receiver_and_callable_identity() {
        let file = parse(
            "actor Counter { func add(value: i32) -> i32 { value } func next() -> i32 { self.add(value = 1) } }\nfunc main() -> i32 { 0 }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower actor calls");
        let result = bodies[&NodeId("function:Counter::next".into())]
            .root
            .result
            .as_ref()
            .expect("method call result");
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("resolved method call expected");
        };
        assert!(matches!(
            &call.callee,
            ResolvedCallee::ActorMethod { actor, method }
                if actor == &NodeId("actor:Counter".into())
                    && method.as_str() == "function:Counter::add"
        ));
        assert_eq!(call.arguments.len(), 2);
        assert!(call.arguments[0].parameter.0 .0.contains("parameter.self"));
        assert!(matches!(
            call.arguments[0].value.kind,
            ResolvedExprKind::Load(_)
        ));
        assert!(call.arguments[1].parameter.0 .0.contains("decl.parameter"));
        bodies[&NodeId("function:Counter::next".into())]
            .validate(program.resolved_types())
            .expect("valid actor method call");
    }

    #[test]
    fn impl_method_call_closes_protocol_and_impl_callable_identities() {
        let file = parse(
            "trait Desc { func describe() -> string }\ntype Dog { name: string }\nimpl Desc for Dog { func describe() -> string { self.name } }\nfunc main() -> string { let dog = Dog { name: \"Rex\" }; dog.describe() }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let bodies = lower_checked_function_bodies(&file, &program).expect("lower impl call");
        let main = &bodies[&NodeId("function:main".into())];
        let result = main.root.result.as_ref().expect("method result");
        let ResolvedExprKind::Call(call) = &result.kind else {
            panic!("resolved protocol method call expected");
        };
        assert!(matches!(
            &call.callee,
            ResolvedCallee::ProtocolMethod { protocol, method }
                if protocol == &NodeId("trait:Desc".into())
                    && method.as_str().starts_with("function:Desc:for:Dog::describe:")
        ));
        assert_eq!(call.arguments.len(), 1);
        assert!(call.arguments[0].parameter.0 .0.contains("parameter.self"));
        assert!(matches!(
            call.arguments[0].value.kind,
            ResolvedExprKind::Load(_)
        ));
        main.validate(program.resolved_types())
            .expect("valid protocol method call");
    }

    #[test]
    fn checked_program_owns_complete_callable_bodies_after_surface_drop() {
        let program = {
            let file = parse(
                "flow Calc { state Zero transition stop(Zero) -> Zero { do { return Zero } } }\nactor Counter { func value() -> i32 { 1 } }\nfunc main() -> i32 { 0 }",
            );
            crate::core::check_program(&file).expect("check")
        };

        let expected = [
            NodeId("function:Counter::value".into()),
            NodeId("function:main".into()),
            NodeId("transition:Calc::stop::Zero".into()),
        ];
        for owner in expected {
            let body = program
                .resolved_body(&owner)
                .unwrap_or_else(|| panic!("missing owned body '{}'", owner.0));
            assert_eq!(body.owner, owner);
            body.validate(program.resolved_types())
                .expect("persisted body remains valid");
        }
    }

    #[test]
    fn local_annotation_and_call_retain_numeric_widening() {
        let file = parse(
            "func identity(value: i64) -> i64 { value }\nfunc main() -> i64 { let widened: i64 = 40; identity(widened + 1) }",
        );
        let program = crate::core::check_program(&file).expect("check widening");
        let body = program
            .resolved_body(&NodeId("function:main".into()))
            .expect("main body");
        let ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(initializer),
        } = &body.root.statements[0].kind
        else {
            panic!("annotated binding expected");
        };
        let ResolvedExprKind::Cast { conversion, .. } = &initializer.kind else {
            panic!("binding must retain an explicit widening");
        };
        assert_eq!(conversion.kind, CheckedConversionKind::NumericWiden);
        assert_eq!(conversion.to, pattern.ty);
        let ResolvedExprKind::Call(call) = &body.root.result.as_ref().unwrap().kind else {
            panic!("typed call expected");
        };
        assert_eq!(call.arguments[0].conversion.to, pattern.ty);
        body.validate(program.resolved_types())
            .expect("valid widening body");
    }

    #[test]
    fn rvalue_aggregate_projection_is_not_forged_into_a_local_place() {
        let file = parse("func first(value: i32) -> i32 { [value][0] }");
        let program = crate::core::check_program(&file).expect("check projection");
        let result = program
            .resolved_body(&NodeId("function:first".into()))
            .unwrap()
            .root
            .result
            .as_ref()
            .unwrap();
        assert!(matches!(
            &result.kind,
            ResolvedExprKind::Project {
                value,
                projection: ResolvedValueProjection::Index(index),
            } if matches!(value.kind, ResolvedExprKind::List(_))
                && matches!(index.kind, ResolvedExprKind::Literal(ResolvedLiteral::Int(0)))
        ));
    }

    #[test]
    fn actor_spawn_and_function_values_have_closed_callable_identities() {
        let file = parse(
            "actor Counter { func value() -> i32 { 1 } }\nfunc identity(value: i32) -> i32 { value }\nfunc main() -> i32 { let handle = Counter.spawn(); let apply = identity; apply(handle.value()) }",
        );
        let program = crate::core::check_program(&file).expect("check callable values");
        let body = program
            .resolved_body(&NodeId("function:main".into()))
            .expect("main body");
        let ResolvedStmtKind::Bind {
            initializer: Some(spawn),
            ..
        } = &body.root.statements[0].kind
        else {
            panic!("spawn binding expected");
        };
        assert!(matches!(
            &spawn.kind,
            ResolvedExprKind::Call(ResolvedCall {
                callee: ResolvedCallee::Builtin(builtin),
                type_arguments,
                ..
            }) if builtin.as_str() == "actor.spawn" && type_arguments == &vec![spawn.ty.clone()]
        ));
        let ResolvedStmtKind::Bind {
            initializer: Some(callable),
            ..
        } = &body.root.statements[1].kind
        else {
            panic!("callable binding expected");
        };
        assert!(matches!(
            &callable.kind,
            ResolvedExprKind::Callable(ResolvedCallee::Function(owner))
                if owner == &NodeId("function:identity".into())
        ));
        body.validate(program.resolved_types())
            .expect("valid closed callable body");
    }

    #[test]
    fn language_methods_resolve_from_canonical_receiver_types() {
        // TOOL-RESOLUTION-001: call-site surface spelling may be Unknown, but
        // typed-body lowering must close language methods from the zonked
        // receiver type rather than defer resolution to a backend.
        let file = parse(
            r#"
func double(value: i32) -> i32 { value * 2 }
func option_status(value: Option<i32>) -> bool { value.is_some() }
func result_map(value: Result<i32, string>) -> Result<i32, string> { value.map(double) }
func shared_value() -> i32 {
    shared value = 1
    let copied = value.clone()
    copied.deref()
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check language methods");

        let tail_call = |owner: &str| {
            let body = program
                .resolved_body(&NodeId(owner.into()))
                .unwrap_or_else(|| panic!("missing body {owner}"));
            let result = body.root.result.as_deref().expect("tail result");
            let ResolvedExprKind::Call(call) = &result.kind else {
                panic!("tail expression is not a call for {owner}");
            };
            call
        };
        let option = tail_call("function:option_status");
        assert!(matches!(
            &option.callee,
            ResolvedCallee::Builtin(id)
                if id.as_str() == "builtin.method.option.is_some"
        ));
        assert_eq!(option.arguments.len(), 1);
        assert_eq!(option.permission, Some(crate::core::Permission::View));

        let result = tail_call("function:result_map");
        assert!(matches!(
            &result.callee,
            ResolvedCallee::Builtin(id) if id.as_str() == "builtin.method.result.map"
        ));
        assert_eq!(result.arguments.len(), 2);
        assert_eq!(result.permission, Some(crate::core::Permission::Consume));

        let shared = tail_call("function:shared_value");
        assert!(matches!(
            &shared.callee,
            ResolvedCallee::Builtin(id) if id.as_str() == "builtin.method.shared.deref"
        ));
        assert_eq!(shared.permission, Some(crate::core::Permission::View));
    }

    #[test]
    fn nested_callable_body_retains_lexical_captures_and_default_identity() {
        let file = parse(
            "func main(base: i32) -> i32 { func add(value: i32 = 2) -> i32 { base + value }; add() }",
        );
        let program = crate::core::check_program(&file).expect("check nested callable");
        let outer = program
            .resolved_body(&NodeId("function:main".into()))
            .expect("outer body");
        let ResolvedStmtKind::NestedCallable(nested_owner) = &outer.root.statements[0].kind else {
            panic!("nested declaration expected");
        };
        let nested = program
            .resolved_body(nested_owner)
            .expect("nested body must be independently persisted");
        assert_eq!(nested.captures.len(), 1);
        assert_eq!(nested.locals[&nested.captures[0]].display_name, "base");
        assert!(nested
            .locals
            .values()
            .any(|local| local.display_name == "value"));
        let signature = program
            .resolved_signature(nested_owner)
            .expect("nested signature");
        assert!(nested
            .default_values
            .contains_key(&signature.parameters[0].id));

        let ResolvedExprKind::Call(call) = &outer.root.result.as_ref().unwrap().kind else {
            panic!("nested call expected");
        };
        assert!(matches!(
            &call.callee,
            ResolvedCallee::Function(owner) if owner == nested_owner
        ));
        assert!(matches!(
            call.arguments[0].value.kind,
            ResolvedExprKind::DefaultArgument { ref callable, .. }
                if callable == nested_owner
        ));
        nested
            .validate(program.resolved_types())
            .expect("valid captured nested body");
    }

    #[test]
    fn nested_callable_capture_catalog_contains_only_used_outer_locals() {
        // RESOURCE-LINEAR-001: lexical environment availability is not proof
        // of capture; only a typed local load closes over the outer resource.
        let file = parse(
            "cap Token\nfunc outer(token: cap Token) -> i32 { func idle() -> i32 { 0 }; drop(token); 0 }\nfunc main() -> i32 { 0 }",
        );
        let program = crate::core::check_program(&file).expect("unused capture is not transferred");
        let outer = program
            .resolved_body(&NodeId("function:outer".into()))
            .expect("outer body");
        let ResolvedStmtKind::NestedCallable(nested_owner) = &outer.root.statements[0].kind else {
            panic!("nested declaration expected");
        };
        let nested = program.resolved_body(nested_owner).expect("nested body");
        assert!(nested.captures.is_empty());
    }

    #[test]
    fn nested_generic_call_has_canonical_instantiation() {
        let file =
            parse("func main() -> i32 { func identity<T>(value: T) -> T { value }; identity(7) }");
        let program = crate::core::check_program(&file).expect("check nested generic");
        let outer = program
            .resolved_body(&NodeId("function:main".into()))
            .expect("outer body");
        let ResolvedStmtKind::NestedCallable(nested_owner) = &outer.root.statements[0].kind else {
            panic!("nested declaration expected");
        };
        assert!(program.resolved_body(nested_owner).is_some());
        let ResolvedExprKind::Call(call) = &outer.root.result.as_ref().unwrap().kind else {
            panic!("nested generic call expected");
        };
        assert_eq!(call.type_arguments.len(), 1);
        assert_eq!(call.type_arguments[0], call.arguments[0].value.ty);
        assert!(matches!(
            &call.callee,
            ResolvedCallee::Function(owner) if owner == nested_owner
        ));
    }
}
