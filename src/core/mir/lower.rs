//! Initial lowering from checker-owned ResolvedBody to canonical MIR.
//!
//! This is intentionally a narrow, fail-closed slice. It proves the
//! architectural boundary for scalar expressions, structured branch control
//! flow, Copy record aggregates, and recursive Move-owned tuple/record product
//! glue shapes (for example `(string, i32)` or `{ name: string, count: i32 }`).
//! The first container slice adds List construction and the `Len`/`Reverse`
//! operations for concrete Copy scalar elements; a narrow generic `Len`
//! facade is materialized only after its specialized body is proven. All
//! other List operations and element shapes remain fail-closed.
//! Unsupported shapes return a structured error and must not
//! silently select the legacy emitter.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::core::ir::{
    NominalTypeId, ResolvedBlock, ResolvedCall, ResolvedCallee, ResolvedExpr, ResolvedExprKind,
    ResolvedPattern, ResolvedPatternKind, ResolvedStmtKind, ResolvedType, ResolvedUnaryOp,
};
use crate::core::{
    CanonicalActionKind, CheckedProgram, NodeId, ResolvedBody, ResolvedLocalId, ResourceAnalysis,
};

use super::types::MirTypeCatalog;
use super::{
    MirAggregateKind, MirBlock, MirBlockId, MirBlockParameter, MirEdgeId, MirFunction,
    MirGenericInstanceContract, MirInstance, MirInstanceId, MirInstruction, MirInstructionId,
    MirInstructionKind, MirOwnershipEvent, MirOwnershipEventKind, MirOwnershipSummary,
    MirSwitchArm, MirSwitchBinding, MirSwitchCase, MirTerminator, MirValue, MirValueId,
    MirVariantPredicate,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirLoweringError {
    pub node_id: NodeId,
    pub message: String,
}

impl std::fmt::Display for MirLoweringError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "cannot lower MIR node '{}': {}",
            self.node_id.0, self.message
        )
    }
}

impl std::error::Error for MirLoweringError {}

/// Lower the currently supported expression/statement subset.
///
/// Supported forms are deliberately small: literals, local loads, unary and
/// binary expressions, calls, casts, binds, expression statements, returns,
/// and branch/match expressions with explicit MIR blocks. Copy-only tuple and
/// record construction/projection/update are also represented, as is the
/// materialized recursive tuple product `(string, i32)` ownership shape.
/// Direct local reads become explicit `Clone` nodes, while root drops become
/// explicit `Drop` nodes.  With a TypeDesc catalog, the narrow
/// ownership-safe record field move becomes `MoveProject`; general partial
/// moves and projected drops remain rejected until their residual contracts
/// are represented in MIR.
pub fn lower_body(body: &ResolvedBody) -> Result<MirFunction, Vec<MirLoweringError>> {
    lower_body_impl(body, None)
}

/// Lower a body with the checker-derived TypeDesc catalog available.  The
/// catalog is used only to choose an already-defined ownership operation;
/// consumers still validate the resulting MIR before execution.
pub fn lower_body_with_type_catalog(
    body: &ResolvedBody,
    type_catalog: &MirTypeCatalog,
) -> Result<MirFunction, Vec<MirLoweringError>> {
    lower_body_impl(body, Some(type_catalog))
}

fn lower_body_impl(
    body: &ResolvedBody,
    type_catalog: Option<&MirTypeCatalog>,
) -> Result<MirFunction, Vec<MirLoweringError>> {
    let mut lowerer = Lowerer {
        body,
        type_catalog,
        values: BTreeMap::new(),
        locals: HashMap::new(),
        blocks: BTreeMap::new(),
        current: MirBlockId::new("bb.entry").expect("static MIR block id"),
        loops: Vec::new(),
        errors: Vec::new(),
    };
    let entry = lowerer.current.clone();
    lowerer.blocks.insert(
        entry.clone(),
        BlockDraft {
            id: entry.clone(),
            parameters: Vec::new(),
            instructions: Vec::new(),
            terminator: None,
        },
    );

    let mut parameters = Vec::with_capacity(body.parameters.len());
    for parameter in &body.parameters {
        let value = lowerer.local_value(parameter)?;
        parameters.push(value);
    }

    lowerer.lower_root(&body.root);
    if !lowerer.current_is_terminated() && lowerer.errors.is_empty() {
        if let Some(result) = body.root.result.as_deref() {
            let value = lowerer.lower_return_expr(result);
            if lowerer.errors.is_empty() {
                lowerer.terminate(MirTerminator::Return { value: Some(value) });
            }
        } else {
            lowerer.terminate(MirTerminator::Return { value: None });
        }
    }

    if !lowerer.errors.is_empty() {
        return Err(lowerer.errors);
    }

    let blocks = lowerer.finish_blocks();
    if !lowerer.errors.is_empty() {
        return Err(lowerer.errors);
    }
    let function = MirFunction {
        owner: body.owner.clone(),
        parameters,
        result: body.root.ty.clone(),
        entry: entry.clone(),
        values: lowerer.values,
        blocks,
        contracts: Vec::new(),
        ownership: MirOwnershipSummary::default(),
    };
    function.validate().map_err(|errors| {
        errors
            .into_iter()
            .map(|error| MirLoweringError {
                node_id: body.owner.clone(),
                message: error.to_string(),
            })
            .collect::<Vec<_>>()
    })?;
    Ok(function)
}

/// Lower one checker-owned callable, including its canonical ownership facts.
/// This is the production entry point used by `lower_program` and tooling;
/// `lower_body` remains useful for focused lowering tests without a resource
/// analysis attachment.
pub fn lower_callable(
    callable: &crate::core::ResolvedCallable,
) -> Result<MirFunction, Vec<MirLoweringError>> {
    let mut function = lower_body(&callable.body)?;
    function.contracts = super::contracts::lower_contracts(callable, &function)?;
    function.ownership = ownership_summary(&callable.resources);
    function.validate().map_err(|errors| {
        errors
            .into_iter()
            .map(|error| MirLoweringError {
                node_id: callable.owner.clone(),
                message: error.to_string(),
            })
            .collect::<Vec<_>>()
    })?;
    Ok(function)
}

/// Lower one callable with the canonical TypeDesc catalog.  This is the
/// production path for ownership-sensitive MIR shapes.
pub fn lower_callable_with_type_catalog(
    callable: &crate::core::ResolvedCallable,
    type_catalog: &MirTypeCatalog,
) -> Result<MirFunction, Vec<MirLoweringError>> {
    let mut function = lower_body_with_type_catalog(&callable.body, type_catalog)?;
    function.contracts = super::contracts::lower_contracts(callable, &function)?;
    function.ownership = ownership_summary(&callable.resources);
    function.validate().map_err(|errors| {
        errors
            .into_iter()
            .map(|error| MirLoweringError {
                node_id: callable.owner.clone(),
                message: error.to_string(),
            })
            .collect::<Vec<_>>()
    })?;
    Ok(function)
}

/// Lower every checker-owned callable that has a ResolvedCallable body.
/// Errors are aggregated so callers can report the complete migration gap in
/// one pass instead of falling back one function at a time.
pub fn lower_program(
    program: &CheckedProgram,
) -> Result<BTreeMap<NodeId, MirFunction>, Vec<MirLoweringError>> {
    let mut lowered = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, callable) in program.callables() {
        // A generic callable declaration is a checker-owned template, not an
        // executable MIR function. Until concrete MIR instantiation exists,
        // putting its unresolved GenericParameter values into the executable
        // graph would make the graph look complete while TypeDesc cannot
        // prove its ABI/glue. Calls to such a template remain visible as
        // calls and are rejected by the canonical call-graph validator.
        if !is_concrete_callable(callable) {
            continue;
        }
        match lower_callable(callable) {
            Ok(function) => {
                lowered.insert(owner.clone(), function);
            }
            Err(mut body_errors) => errors.append(&mut body_errors),
        }
    }
    if errors.is_empty() {
        Ok(lowered)
    } else {
        Err(errors)
    }
}

/// Lower every callable with the canonical TypeDesc catalog.  This is the
/// production entry point used by `MirProgram`; all ownership-sensitive
/// projection decisions therefore use checker-derived facts before backend
/// emission.
pub fn lower_program_with_type_catalog(
    program: &CheckedProgram,
    type_catalog: &MirTypeCatalog,
) -> Result<BTreeMap<NodeId, MirFunction>, Vec<MirLoweringError>> {
    let mut lowered = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, callable) in program.callables() {
        // Do not lower a polymorphic template as if it were a concrete
        // function. A concrete instance table is a separate MIR contract;
        // until it exists, the only sound behavior is to omit the template
        // and let any call to it fail closed in validate_call_graph().
        if !is_concrete_callable(callable) {
            continue;
        }
        match lower_callable_with_type_catalog(callable, type_catalog) {
            Ok(function) => {
                lowered.insert(owner.clone(), function);
            }
            Err(mut body_errors) => errors.append(&mut body_errors),
        }
    }
    if errors.is_empty() {
        Ok(lowered)
    } else {
        Err(errors)
    }
}

/// Materialize the first concrete generic MIR instance family.
///
/// This is intentionally a closed contract family, not a backend-specific
/// monomorphization shortcut: checker-selected type arguments are carried by
/// the MIR `Call`, the instance table records the template/arguments proof,
/// and the executable function is already specialized MIR. The admitted
/// families are scalar/flat-variant identity, owned String identity, scalar
/// Set facades, and the concrete Copy-scalar List `Len` facade. All other
/// generic bodies remain fail-closed.
pub fn materialize_concrete_generic_instances(
    program: &CheckedProgram,
    type_catalog: &MirTypeCatalog,
    functions: &mut BTreeMap<NodeId, MirFunction>,
) -> Result<BTreeMap<MirInstanceId, MirInstance>, Vec<MirLoweringError>> {
    materialize_concrete_generic_instances_excluding_sources(
        program,
        type_catalog,
        functions,
        &HashSet::new(),
    )
}

/// Compatibility-source variant. A generic callable from an excluded source
/// cannot become an executable MIR instance by accident.
pub fn materialize_concrete_generic_instances_excluding_sources(
    program: &CheckedProgram,
    type_catalog: &MirTypeCatalog,
    functions: &mut BTreeMap<NodeId, MirFunction>,
    excluded_sources: &HashSet<crate::span::SourceId>,
) -> Result<BTreeMap<MirInstanceId, MirInstance>, Vec<MirLoweringError>> {
    let mut requests: BTreeMap<
        (NodeId, Vec<crate::core::ResolvedTypeId>),
        Vec<(NodeId, MirBlockId, usize, MirInstructionId)>,
    > = BTreeMap::new();
    let mut errors = Vec::new();

    for (caller, function) in functions.iter() {
        for (block_id, block) in &function.blocks {
            for (index, instruction) in block.instructions.iter().enumerate() {
                let MirInstructionKind::Call {
                    callee: ResolvedCallee::Function(template),
                    type_arguments,
                    ..
                } = &instruction.kind
                else {
                    continue;
                };
                if type_arguments.is_empty() {
                    continue;
                }
                let Some(callable) = program.callable(template) else {
                    continue;
                };
                if callable.signature.generic_parameters.is_empty() {
                    continue;
                }
                if excluded_sources.contains(&callable.body.root.origin.user_span().source_id) {
                    errors.push(MirLoweringError {
                        node_id: NodeId(instruction.id.as_str().to_owned()),
                        message: format!(
                            "generic callee '{}' belongs to an excluded source and has no canonical MIR instance",
                            template.0
                        ),
                    });
                    continue;
                }
                requests
                    .entry((template.clone(), type_arguments.clone()))
                    .or_default()
                    .push((
                        caller.clone(),
                        block_id.clone(),
                        index,
                        instruction.id.clone(),
                    ));
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut instances = BTreeMap::new();
    for ((template, arguments), sites) in requests {
        let callable = program.callable(&template).ok_or_else(|| {
            vec![MirLoweringError {
                node_id: sites
                    .first()
                    .map(|(_, _, _, instruction)| NodeId(instruction.as_str().to_owned()))
                    .unwrap_or_else(|| template.clone()),
                message: format!(
                    "generic template '{}' is absent from checker catalog",
                    template.0
                ),
            }]
        })?;
        let (instance, function) = materialize_generic_instance(
            callable,
            &template,
            &arguments,
            program,
            type_catalog,
            sites.first().map(|(_, _, _, instruction)| instruction),
        )?;
        let target = instance.function.clone();
        if functions.insert(target.clone(), function).is_some() {
            return Err(vec![MirLoweringError {
                node_id: template.clone(),
                message: format!(
                    "generic MIR instance '{}' conflicts with an existing executable function",
                    instance.id
                ),
            }]);
        }
        instances.insert(instance.id.clone(), instance);

        for (caller, block_id, index, instruction_id) in sites {
            let Some(function) = functions.get_mut(&caller) else {
                return Err(vec![MirLoweringError {
                    node_id: NodeId(instruction_id.as_str().to_owned()),
                    message: format!(
                        "generic call caller '{}' is absent from MIR graph",
                        caller.0
                    ),
                }]);
            };
            let Some(instruction) = function
                .blocks
                .get_mut(&block_id)
                .and_then(|block| block.instructions.get_mut(index))
            else {
                return Err(vec![MirLoweringError {
                    node_id: NodeId(instruction_id.as_str().to_owned()),
                    message: "generic call site disappeared before instance rewrite".into(),
                }]);
            };
            let MirInstructionKind::Call {
                callee,
                variant_call_contract,
                ..
            } = &mut instruction.kind
            else {
                return Err(vec![MirLoweringError {
                    node_id: NodeId(instruction_id.as_str().to_owned()),
                    message: "generic call site is no longer a MIR Call".into(),
                }]);
            };
            *callee = ResolvedCallee::Function(target.clone());
            if let Some(receipt) = variant_call_contract {
                // S80 materialized the receipt while the call still named
                // the generic template.  Once this pass installs the
                // specialized executable function, the receipt must follow
                // that canonical target identity rather than retain a stale
                // template owner.
                receipt.callee = target.clone();
            }
        }
    }
    Ok(instances)
}

fn materialize_generic_instance(
    callable: &crate::core::ResolvedCallable,
    template: &NodeId,
    arguments: &[crate::core::ResolvedTypeId],
    program: &CheckedProgram,
    type_catalog: &MirTypeCatalog,
    instruction: Option<&MirInstructionId>,
) -> Result<(MirInstance, MirFunction), Vec<MirLoweringError>> {
    let subject = || {
        instruction
            .map(|instruction| NodeId(instruction.as_str().to_owned()))
            .unwrap_or_else(|| template.clone())
    };
    if callable.signature.generic_parameters.len() != 1 || arguments.len() != 1 {
        return Err(vec![MirLoweringError {
            node_id: subject(),
            message:
                "canonical generic MIR instances require one type parameter and one scalar argument"
                    .into(),
        }]);
    }
    let generic_id = generic_parameter_type_id(program, &callable.signature.generic_parameters[0])
        .ok_or_else(|| {
            vec![MirLoweringError {
                node_id: subject(),
                message: "generic signature parameter has no canonical ResolvedTypeId".into(),
            }]
        })?;
    let generic_list_facade = callable.signature.parameters.iter().any(|parameter| {
        mentions_generic_list_type(program, &parameter.ty, &generic_id, &mut HashSet::new())
    }) || mentions_generic_list_type(
        program,
        &callable.signature.result,
        &generic_id,
        &mut HashSet::new(),
    );
    let generic_set_facade = callable.signature.parameters.iter().any(|parameter| {
        mentions_generic_set_type(program, &parameter.ty, &generic_id, &mut HashSet::new())
    }) || mentions_generic_set_type(
        program,
        &callable.signature.result,
        &generic_id,
        &mut HashSet::new(),
    );
    let is_identity = callable.signature.parameters.len() == 1
        && callable.signature.parameters[0].ty == generic_id
        && callable.signature.result == generic_id;
    let concrete = arguments.first().cloned().ok_or_else(|| {
        vec![MirLoweringError {
            node_id: subject(),
            message: "generic instance has no concrete argument".into(),
        }]
    })?;
    let is_owned_string_identity =
        is_identity && type_catalog.validate_owned_string(&concrete).is_ok();
    let validate_arguments = if is_identity {
        MirTypeCatalog::validate_generic_identity_arguments
    } else {
        MirTypeCatalog::validate_scalar_generic_arguments
    };
    validate_arguments(type_catalog, arguments).map_err(|message| {
        vec![MirLoweringError {
            node_id: subject(),
            message: format!(
                "generic MIR instance argument is outside scalar contract or flat Copy variant contract: {message}"
            ),
        }]
    })?;
    // Generic List operations need their checker-derived receipt while the
    // polymorphic body is lowered. The receipt is specialized below after
    // every MIR value has received its concrete TypeDesc identity.
    let mut function =
        lower_callable_with_type_catalog(callable, type_catalog).map_err(|mut errors| {
            for error in &mut errors {
                error.node_id = subject();
            }
            errors
        })?;
    if is_identity && !is_owned_string_identity {
        if let Err(message) = super::validate_generic_identity_shape(&function, &generic_id) {
            return Err(vec![MirLoweringError {
                node_id: subject(),
                message,
            }]);
        }
    }
    let instance_id = MirInstanceId::for_template(template, arguments).map_err(|error| {
        vec![MirLoweringError {
            node_id: subject(),
            message: error.to_string(),
        }]
    })?;
    let function_owner = NodeId(format!("function:mir:{}", instance_id.as_str()));
    function.owner = function_owner.clone();
    function.result = specialize_type_id(&function.result, &generic_id, &concrete, program)
        .map_err(|message| {
            vec![MirLoweringError {
                node_id: subject(),
                message,
            }]
        })?;
    let mut specialization_errors = Vec::new();
    for value in function.values.values_mut() {
        match specialize_type_id(&value.ty, &generic_id, &concrete, program) {
            Ok(ty) => value.ty = ty,
            Err(message) => specialization_errors.push(MirLoweringError {
                node_id: subject(),
                message,
            }),
        }
    }
    if !specialization_errors.is_empty() {
        return Err(specialization_errors);
    }
    let list_operations = function
        .blocks
        .iter()
        .flat_map(|(block_id, block)| {
            block
                .instructions
                .iter()
                .enumerate()
                .filter_map(|(index, instruction)| match &instruction.kind {
                    MirInstructionKind::ListOp {
                        result,
                        list,
                        argument,
                        operation,
                        list_operation_contract: Some(_),
                    } => Some((
                        block_id.clone(),
                        index,
                        result.clone(),
                        list.clone(),
                        argument.clone(),
                        *operation,
                    )),
                    _ => None,
                })
        })
        .collect::<Vec<_>>();
    for (block_id, instruction_index, result, list, argument, operation) in list_operations {
        let result_ty = function
            .values
            .get(&result)
            .map(|value| value.ty.clone())
            .ok_or_else(|| {
                vec![MirLoweringError {
                    node_id: subject(),
                    message: "generic List facade result has no specialized TypeDesc".into(),
                }]
            })?;
        let list_ty = function
            .values
            .get(&list)
            .map(|value| value.ty.clone())
            .ok_or_else(|| {
                vec![MirLoweringError {
                    node_id: subject(),
                    message: "generic List facade receiver has no specialized TypeDesc".into(),
                }]
            })?;
        let argument_ty = argument
            .as_ref()
            .map(|value| {
                function
                    .values
                    .get(value)
                    .map(|info| info.ty.clone())
                    .ok_or_else(|| {
                        vec![MirLoweringError {
                            node_id: subject(),
                            message: "generic List facade argument has no specialized TypeDesc"
                                .into(),
                        }]
                    })
            })
            .transpose()?;
        let receipt = type_catalog
            .validated_list_operation_contract_with_argument(
                &result_ty,
                &list_ty,
                argument_ty.as_ref(),
                operation,
            )
            .map_err(|message| {
                vec![MirLoweringError {
                    node_id: subject(),
                    message: format!(
                        "generic List facade receipt specialization failed: {message}"
                    ),
                }]
            })?;
        let instruction = function
            .blocks
            .get_mut(&block_id)
            .and_then(|block| block.instructions.get_mut(instruction_index))
            .ok_or_else(|| {
                vec![MirLoweringError {
                    node_id: subject(),
                    message: "generic List facade operation disappeared during specialization"
                        .into(),
                }]
            })?;
        let MirInstructionKind::ListOp {
            list_operation_contract,
            ..
        } = &mut instruction.kind
        else {
            return Err(vec![MirLoweringError {
                node_id: subject(),
                message: "generic List facade operation changed during specialization".into(),
            }]);
        };
        *list_operation_contract = Some(receipt);
    }
    if is_owned_string_identity {
        let block = function.blocks.get_mut(&function.entry).ok_or_else(|| {
            vec![MirLoweringError {
                node_id: subject(),
                message: "owned String generic identity entry block is absent".into(),
            }]
        })?;
        let source = match block.instructions.as_slice() {
            [MirInstruction {
                kind: MirInstructionKind::Clone { source, .. },
                ..
            }] => Ok(source.clone()),
            [_] => Err("owned String generic identity specialization must clone its parameter"),
            _ => Err("owned String generic identity specialization requires one Clone instruction"),
        };
        let source = match source {
            Ok(source) => source,
            Err(message) => {
                return Err(vec![MirLoweringError {
                    node_id: subject(),
                    message: message.into(),
                }])
            }
        };
        let drop_id = MirInstructionId::new(format!(
            "inst:drop:owned-string-identity:{}",
            function.owner.0
        ))
        .map_err(|error| {
            vec![MirLoweringError {
                node_id: subject(),
                message: error.to_string(),
            }]
        })?;
        block.instructions.push(MirInstruction {
            id: drop_id,
            kind: MirInstructionKind::Drop { value: source },
        });
    }
    let contract = if is_identity {
        if type_catalog.validate_owned_string(&concrete).is_ok() {
            MirGenericInstanceContract::OwnedStringIdentity
        } else {
            MirGenericInstanceContract::ScalarIdentity
        }
    } else if generic_set_facade {
        let operation = detect_scalar_set_facade_operation(&function, type_catalog, &subject())?;
        MirGenericInstanceContract::ScalarSetFacade { operation }
    } else if generic_list_facade
        || function.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction.kind, MirInstructionKind::ListOp { .. }))
        })
    {
        let operation = detect_scalar_list_facade_operation(&function, type_catalog, &subject())?;
        MirGenericInstanceContract::ScalarListFacade { operation }
    } else {
        let operation = detect_scalar_set_facade_operation(&function, type_catalog, &subject())?;
        MirGenericInstanceContract::ScalarSetFacade { operation }
    };
    if is_owned_string_identity {
        if let Err(message) = super::validate_owned_string_identity_shape(&function, &concrete) {
            return Err(vec![MirLoweringError {
                node_id: subject(),
                message,
            }]);
        }
    }
    function.validate().map_err(|errors| {
        errors
            .into_iter()
            .map(|error| MirLoweringError {
                node_id: subject(),
                message: error.to_string(),
            })
            .collect::<Vec<_>>()
    })?;
    validate_arguments(type_catalog, arguments).map_err(|message| {
        vec![MirLoweringError {
            node_id: subject(),
            message: format!(
                "specialized generic TypeDesc is outside scalar contract or flat Copy variant contract: {message}"
            ),
        }]
    })?;
    let instance = MirInstance {
        id: instance_id,
        template: template.clone(),
        arguments: arguments.to_vec(),
        function: function_owner,
        contract,
    };
    Ok((instance, function))
}

fn detect_scalar_list_facade_operation(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    subject: &NodeId,
) -> Result<super::MirListOperation, Vec<MirLoweringError>> {
    let operations = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match instruction.kind {
            MirInstructionKind::ListOp { operation, .. } => Some(operation),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [operation] = operations.as_slice() else {
        return Err(vec![MirLoweringError {
            node_id: subject.clone(),
            message: "generic List facade must lower to exactly one canonical ListOp".into(),
        }]);
    };
    if *operation != super::MirListOperation::Len {
        return Err(vec![MirLoweringError {
            node_id: subject.clone(),
            message: "generic List facade only admits canonical ListOp::Len".into(),
        }]);
    }
    validate_scalar_list_facade_mir(function, type_catalog, *operation).map_err(|message| {
        vec![MirLoweringError {
            node_id: subject.clone(),
            message: format!("generic List facade contract is invalid: {message}"),
        }]
    })?;
    Ok(*operation)
}

/// Validate the concrete body of the generic List `Len` facade. The body is
/// deliberately structural: one List parameter is cloned exactly once, the
/// clone is the receiver of one receipt-bearing `ListOp::Len`, and that
/// result is returned. This keeps a specialized generic function from hiding
/// an arbitrary body behind a canonical-looking instance symbol.
pub(crate) fn validate_scalar_list_facade_mir(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    operation: super::MirListOperation,
) -> Result<(), String> {
    if operation != super::MirListOperation::Len {
        return Err("scalar List facade only admits ListOp::Len".into());
    }
    let [list_parameter] = function.parameters.as_slice() else {
        return Err("scalar List facade must have exactly one List parameter".into());
    };
    let list_ty = function
        .values
        .get(list_parameter)
        .ok_or_else(|| "scalar List facade receiver parameter is absent".to_string())?
        .ty
        .clone();
    let Some(super::types::MirLayout::List { .. }) = type_catalog
        .get(&list_ty)
        .map(|descriptor| &descriptor.layout)
    else {
        return Err("scalar List facade receiver is not a canonical List<T>".into());
    };
    let Some(block) = function.blocks.get(&function.entry) else {
        return Err("scalar List facade entry block is absent".into());
    };
    if function.blocks.len() != 1 {
        return Err("scalar List facade must have exactly one MIR block".into());
    }
    let mut list_op = None;
    let mut clones = Vec::new();
    for instruction in &block.instructions {
        match &instruction.kind {
            MirInstructionKind::Clone { result, source } => {
                clones.push((result.clone(), source.clone()));
            }
            MirInstructionKind::ListOp {
                result,
                operation: actual,
                list,
                argument,
                list_operation_contract,
            } => {
                if *actual != operation {
                    return Err("scalar List facade contains a different ListOp operation".into());
                }
                if list_op
                    .replace((
                        result.clone(),
                        list.clone(),
                        argument.clone(),
                        list_operation_contract.clone(),
                    ))
                    .is_some()
                {
                    return Err("scalar List facade must contain exactly one ListOp".into());
                }
            }
            _ => return Err(
                "scalar List facade body may contain only parameter Clone and ListOp instructions"
                    .into(),
            ),
        }
    }
    let Some((list_result, list_operand, argument, receipt)) = list_op else {
        return Err("scalar List facade must contain exactly one ListOp".into());
    };
    if clones.len() != 1
        || clones
            .first()
            .is_none_or(|(_, source)| *source != *list_parameter)
    {
        return Err("scalar List facade must clone its List parameter exactly once".into());
    }
    if list_operand != clones[0].0 || argument.is_some() {
        return Err(
            "scalar List facade ListOp must read the cloned List without an argument".into(),
        );
    }
    let receipt =
        receipt.ok_or_else(|| "scalar List facade ListOp has no canonical receipt".to_string())?;
    let result_ty = function
        .values
        .get(&list_result)
        .ok_or_else(|| "scalar List facade result value is absent".to_string())?
        .ty
        .clone();
    type_catalog.validate_list_operation_receipt(&result_ty, &list_ty, operation, &receipt)?;
    let MirTerminator::Return {
        value: Some(returned),
    } = &block.terminator
    else {
        return Err("scalar List facade must return its ListOp result".into());
    };
    if returned != &list_result {
        return Err("scalar List facade return value is not the ListOp result".into());
    }
    Ok(())
}

/// Substitute one checker-owned generic parameter in the small set of
/// parameterized nominal types used by the current concrete Set facade. The
/// returned identity is looked up in the original resolved type table; a
/// backend never invents a new type id from a display name or ABI spelling.
fn specialize_type_id(
    id: &crate::core::ResolvedTypeId,
    generic_id: &crate::core::ResolvedTypeId,
    concrete: &crate::core::ResolvedTypeId,
    program: &CheckedProgram,
) -> Result<crate::core::ResolvedTypeId, String> {
    if id == generic_id {
        return Ok(concrete.clone());
    }
    let Some(ResolvedType::Nominal {
        item,
        arguments,
        is_linear,
    }) = program.resolved_types().get(id)
    else {
        return Ok(id.clone());
    };
    let mut specialized_arguments = Vec::with_capacity(arguments.len());
    let mut changed = false;
    for argument in arguments {
        let specialized = specialize_type_id(argument, generic_id, concrete, program)?;
        changed |= specialized != *argument;
        specialized_arguments.push(specialized);
    }
    if !changed {
        return Ok(id.clone());
    }
    let specialized = ResolvedType::Nominal {
        item: item.clone(),
        arguments: specialized_arguments,
        is_linear: *is_linear,
    };
    program
        .resolved_types()
        .iter()
        .find_map(|(candidate, candidate_ty)| {
            (candidate_ty == &specialized).then(|| candidate.clone())
        })
        .ok_or_else(|| {
            format!(
                "generic MIR specialization has no canonical type identity for '{}<...>'",
                item.as_str()
            )
        })
}

fn detect_scalar_set_facade_operation(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    subject: &NodeId,
) -> Result<super::MirSetOperation, Vec<MirLoweringError>> {
    let operations = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match instruction.kind {
            MirInstructionKind::SetOp { operation, .. } => Some(operation),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [operation] = operations.as_slice() else {
        return Err(vec![MirLoweringError {
            node_id: subject.clone(),
            message: "generic Set facade must lower to exactly one canonical SetOp".into(),
        }]);
    };
    validate_scalar_set_facade_mir(function, type_catalog, *operation).map_err(|message| {
        vec![MirLoweringError {
            node_id: subject.clone(),
            message: format!("generic Set facade contract is invalid: {message}"),
        }]
    })?;
    Ok(*operation)
}

/// Validate the concrete body of a generic Set facade. This is deliberately
/// structural: a materialized instance may contain only parameter clones and
/// one canonical SetOp whose operation/result/argument contract is proven by
/// TypeDesc. That prevents a generic instance from becoming an unverified
/// legacy-style body hidden behind a specialized symbol.
pub(crate) fn validate_scalar_set_facade_mir(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    operation: super::MirSetOperation,
) -> Result<(), String> {
    let [set_parameter, rest @ ..] = function.parameters.as_slice() else {
        return Err("scalar Set facade must have a Set parameter".into());
    };
    if rest.len() > 1 {
        return Err("scalar Set facade has too many parameters".into());
    }
    let set_ty = function
        .values
        .get(set_parameter)
        .ok_or_else(|| "scalar Set facade receiver parameter is absent".to_string())?
        .ty
        .clone();
    let element_ty = match type_catalog.get(&set_ty).map(|desc| &desc.layout) {
        Some(super::types::MirLayout::Set { element }) => element.clone(),
        _ => return Err("scalar Set facade receiver is not a canonical Set<T>".into()),
    };
    if rest.len() == 1 {
        let argument_ty = function
            .values
            .get(&rest[0])
            .ok_or_else(|| "scalar Set facade argument parameter is absent".to_string())?
            .ty
            .clone();
        if argument_ty != element_ty {
            return Err("scalar Set facade argument does not match Set element identity".into());
        }
    }

    let Some(block) = function.blocks.get(&function.entry) else {
        return Err("scalar Set facade entry block is absent".into());
    };
    if function.blocks.len() != 1 {
        return Err("scalar Set facade must have exactly one MIR block".into());
    }
    let mut set_op = None;
    for instruction in &block.instructions {
        match &instruction.kind {
            MirInstructionKind::SetOp {
                result,
                operation: actual,
                set,
                argument,
            } => {
                if *actual != operation {
                    return Err("scalar Set facade contains a different SetOp operation".into());
                }
                if set_op
                    .replace((result.clone(), set.clone(), argument.clone()))
                    .is_some()
                {
                    return Err("scalar Set facade must contain exactly one SetOp".into());
                }
            }
            MirInstructionKind::Clone { .. } => {}
            _ => return Err(
                "scalar Set facade body may contain only parameter Clone and SetOp instructions"
                    .into(),
            ),
        }
    }
    let Some((set_result, set_operand, set_argument)) = set_op else {
        return Err("scalar Set facade must contain exactly one SetOp".into());
    };
    let clones = block
        .instructions
        .iter()
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::Clone { result, source } => Some((result, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if clones.len() != function.parameters.len()
        || clones.iter().any(|(_, source)| {
            !function
                .parameters
                .iter()
                .any(|parameter| parameter == *source)
        })
        || clones
            .iter()
            .map(|(_, source)| *source)
            .collect::<HashSet<_>>()
            .len()
            != clones.len()
    {
        return Err(
            "scalar Set facade must clone each callable parameter exactly once before SetOp".into(),
        );
    }
    let clone_for = |source: &MirValueId| {
        clones
            .iter()
            .find_map(|(result, candidate)| (*candidate == source).then_some((*result).clone()))
    };
    if Some(set_operand.clone()) != clone_for(set_parameter) {
        return Err("scalar Set facade SetOp receiver is not the cloned Set parameter".into());
    }
    let expects_argument = matches!(
        operation,
        super::MirSetOperation::Contains
            | super::MirSetOperation::Insert
            | super::MirSetOperation::Remove
    );
    if expects_argument {
        let Some(parameter) = rest.first() else {
            return Err("scalar Set facade operation requires an element argument".into());
        };
        if set_argument.as_ref() != clone_for(parameter).as_ref() {
            return Err(
                "scalar Set facade SetOp argument is not the cloned element parameter".into(),
            );
        }
    } else if set_argument.is_some() {
        return Err("scalar Set facade read operation unexpectedly has an argument".into());
    }
    let MirTerminator::Return {
        value: Some(returned),
    } = &block.terminator
    else {
        return Err("scalar Set facade must return its SetOp result".into());
    };
    if returned != &set_result {
        return Err("scalar Set facade return value is not the SetOp result".into());
    }
    let result_ty = function
        .values
        .get(&set_result)
        .ok_or_else(|| "scalar Set facade result value is absent".to_string())?
        .ty
        .clone();
    type_catalog.validate_set_operation(
        &result_ty,
        &set_ty,
        set_argument
            .as_ref()
            .and_then(|argument| function.values.get(argument))
            .map(|value| &value.ty),
        operation,
    )
}

fn generic_parameter_type_id(
    program: &CheckedProgram,
    parameter: &NodeId,
) -> Option<crate::core::ResolvedTypeId> {
    program
        .resolved_types()
        .iter()
        .find_map(|(id, ty)| match ty {
            crate::core::ResolvedType::GenericParameter(candidate) if candidate == parameter => {
                Some(id.clone())
            }
            _ => None,
        })
}

fn mentions_generic_list_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    generic_id: &crate::core::ResolvedTypeId,
    seen: &mut HashSet<crate::core::ResolvedTypeId>,
) -> bool {
    mentions_generic_container_type(program, id, generic_id, "builtin:type:List", seen)
}

fn mentions_generic_set_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    generic_id: &crate::core::ResolvedTypeId,
    seen: &mut HashSet<crate::core::ResolvedTypeId>,
) -> bool {
    mentions_generic_container_type(program, id, generic_id, "builtin:type:Set", seen)
}

fn mentions_generic_container_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    generic_id: &crate::core::ResolvedTypeId,
    container_item: &str,
    seen: &mut HashSet<crate::core::ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return false;
    }
    match program.resolved_types().get(id) {
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) => {
            if item.as_str() == container_item {
                arguments.iter().any(|argument| {
                    contains_generic_type(program, argument, generic_id, &mut HashSet::new())
                })
            } else {
                arguments.iter().any(|argument| {
                    mentions_generic_container_type(
                        program,
                        argument,
                        generic_id,
                        container_item,
                        seen,
                    )
                })
            }
        }
        Some(ResolvedType::Option(inner))
        | Some(ResolvedType::CBuffer(inner))
        | Some(ResolvedType::Ownership { target: inner, .. })
        | Some(ResolvedType::Newtype { inner, .. })
        | Some(ResolvedType::Slice(inner))
        | Some(ResolvedType::RawPointer { target: inner, .. }) => {
            mentions_generic_container_type(program, inner, generic_id, container_item, seen)
        }
        Some(ResolvedType::Result { ok, error }) => {
            mentions_generic_container_type(program, ok, generic_id, container_item, seen)
                || mentions_generic_container_type(program, error, generic_id, container_item, seen)
        }
        Some(ResolvedType::Tuple(items)) => items.iter().any(|item| {
            mentions_generic_container_type(program, item, generic_id, container_item, seen)
        }),
        Some(ResolvedType::Array { element, .. }) => {
            mentions_generic_container_type(program, element, generic_id, container_item, seen)
        }
        Some(ResolvedType::Function {
            parameters, result, ..
        }) => {
            parameters.iter().any(|parameter| {
                mentions_generic_container_type(
                    program,
                    parameter,
                    generic_id,
                    container_item,
                    seen,
                )
            }) || mentions_generic_container_type(program, result, generic_id, container_item, seen)
        }
        _ => false,
    }
}

fn contains_generic_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    generic_id: &crate::core::ResolvedTypeId,
    seen: &mut HashSet<crate::core::ResolvedTypeId>,
) -> bool {
    if id == generic_id || !seen.insert(id.clone()) {
        return id == generic_id;
    }
    match program.resolved_types().get(id) {
        Some(ResolvedType::Nominal { arguments, .. }) => arguments
            .iter()
            .any(|argument| contains_generic_type(program, argument, generic_id, seen)),
        Some(ResolvedType::Option(inner))
        | Some(ResolvedType::CBuffer(inner))
        | Some(ResolvedType::Ownership { target: inner, .. })
        | Some(ResolvedType::Newtype { inner, .. })
        | Some(ResolvedType::Slice(inner))
        | Some(ResolvedType::RawPointer { target: inner, .. }) => {
            contains_generic_type(program, inner, generic_id, seen)
        }
        Some(ResolvedType::Result { ok, error }) => {
            contains_generic_type(program, ok, generic_id, seen)
                || contains_generic_type(program, error, generic_id, seen)
        }
        Some(ResolvedType::Tuple(items)) => items
            .iter()
            .any(|item| contains_generic_type(program, item, generic_id, seen)),
        Some(ResolvedType::Array { element, .. }) => {
            contains_generic_type(program, element, generic_id, seen)
        }
        Some(ResolvedType::Function {
            parameters, result, ..
        }) => {
            parameters
                .iter()
                .any(|parameter| contains_generic_type(program, parameter, generic_id, seen))
                || contains_generic_type(program, result, generic_id, seen)
        }
        _ => false,
    }
}

fn is_concrete_callable(callable: &crate::core::ResolvedCallable) -> bool {
    callable.signature.generic_parameters.is_empty()
}

fn ownership_summary(analysis: &ResourceAnalysis) -> MirOwnershipSummary {
    MirOwnershipSummary {
        events: analysis
            .actions
            .iter()
            .map(|action| MirOwnershipEvent {
                kind: match action.kind {
                    CanonicalActionKind::Read => MirOwnershipEventKind::Read,
                    CanonicalActionKind::Write => MirOwnershipEventKind::Write,
                    CanonicalActionKind::Introduce => MirOwnershipEventKind::Introduce,
                    CanonicalActionKind::Move => MirOwnershipEventKind::Move,
                    CanonicalActionKind::Drop => MirOwnershipEventKind::Drop,
                    CanonicalActionKind::Return => MirOwnershipEventKind::Return,
                    CanonicalActionKind::TransferSession => MirOwnershipEventKind::TransferSession,
                    CanonicalActionKind::TransferChild => MirOwnershipEventKind::TransferChild,
                    CanonicalActionKind::BorrowShared => MirOwnershipEventKind::BorrowShared,
                    CanonicalActionKind::BorrowMut => MirOwnershipEventKind::BorrowMut,
                    CanonicalActionKind::BorrowEnd => MirOwnershipEventKind::BorrowEnd,
                },
                resource: action.resource.0 .0.clone(),
                value: action
                    .resource
                    .0
                     .0
                    .ends_with("/local")
                    .then(|| MirValueId::new(format!("local:{}", action.resource.0 .0)))
                    .and_then(Result::ok),
                source: action.source.as_ref().map(|place| place.display()),
                target: action.target.as_ref().map(|place| place.display()),
                point: action.location.point.clone(),
            })
            .collect(),
    }
}

struct BlockDraft {
    id: MirBlockId,
    parameters: Vec<MirBlockParameter>,
    instructions: Vec<MirInstruction>,
    terminator: Option<MirTerminator>,
}

struct LoopTargets {
    header: MirBlockId,
    exit: MirBlockId,
}

struct Lowerer<'a> {
    body: &'a ResolvedBody,
    type_catalog: Option<&'a MirTypeCatalog>,
    values: BTreeMap<MirValueId, MirValue>,
    locals: HashMap<ResolvedLocalId, MirValueId>,
    blocks: BTreeMap<MirBlockId, BlockDraft>,
    current: MirBlockId,
    loops: Vec<LoopTargets>,
    errors: Vec<MirLoweringError>,
}

impl<'a> Lowerer<'a> {
    fn error(&mut self, node_id: &NodeId, message: impl Into<String>) {
        self.errors.push(MirLoweringError {
            node_id: node_id.clone(),
            message: message.into(),
        });
    }

    fn id(&mut self, prefix: &str, node_id: &NodeId) -> Option<MirValueId> {
        match super::MirValueId::new(format!("{prefix}:{}", node_id.0)) {
            Ok(id) => Some(id),
            Err(error) => {
                self.error(node_id, error.to_string());
                None
            }
        }
    }

    fn instruction_id(&mut self, node_id: &NodeId, role: &str) -> Option<MirInstructionId> {
        match super::MirInstructionId::new(format!("inst:{role}:{}", node_id.0)) {
            Ok(id) => Some(id),
            Err(error) => {
                self.error(node_id, error.to_string());
                None
            }
        }
    }

    fn current_is_terminated(&self) -> bool {
        self.blocks
            .get(&self.current)
            .and_then(|block| block.terminator.as_ref())
            .is_some()
    }

    fn block_id(&mut self, role: &str, node_id: &NodeId) -> Option<MirBlockId> {
        match MirBlockId::new(format!("bb:{role}:{}", node_id.0)) {
            Ok(id) => Some(id),
            Err(error) => {
                self.error(node_id, error.to_string());
                None
            }
        }
    }

    fn edge_id(&mut self, role: &str, node_id: &NodeId) -> Option<MirEdgeId> {
        match MirEdgeId::new(format!("edge:{role}:{}", node_id.0)) {
            Ok(id) => Some(id),
            Err(error) => {
                self.error(node_id, error.to_string());
                None
            }
        }
    }

    fn add_block(&mut self, id: MirBlockId, parameters: Vec<MirBlockParameter>) {
        if self.blocks.contains_key(&id) {
            self.error(
                &NodeId(id.as_str().to_string()),
                "MIR block identity is generated more than once",
            );
            return;
        }
        self.blocks.insert(
            id.clone(),
            BlockDraft {
                id,
                parameters,
                instructions: Vec::new(),
                terminator: None,
            },
        );
    }

    fn switch_to(&mut self, id: MirBlockId) {
        if self.blocks.contains_key(&id) {
            self.current = id;
        } else {
            self.error(
                &NodeId(self.body.owner.0.clone()),
                "attempted to switch to a missing MIR block",
            );
        }
    }

    fn terminate(&mut self, terminator: MirTerminator) {
        let block_id = self.current.clone();
        let already_terminated = self
            .blocks
            .get(&block_id)
            .is_some_and(|block| block.terminator.is_some());
        if already_terminated {
            self.error(
                &NodeId(block_id.as_str().to_string()),
                "MIR block receives more than one terminator",
            );
        } else if let Some(block) = self.blocks.get_mut(&block_id) {
            block.terminator = Some(terminator);
        } else {
            self.error(
                &NodeId(block_id.as_str().to_string()),
                "cannot terminate a missing MIR block",
            );
        }
    }

    fn finish_blocks(&mut self) -> BTreeMap<MirBlockId, MirBlock> {
        let mut finished = BTreeMap::new();
        for (id, draft) in std::mem::take(&mut self.blocks) {
            let Some(terminator) = draft.terminator else {
                self.error(
                    &NodeId(id.as_str().to_string()),
                    "MIR block has no terminator",
                );
                continue;
            };
            finished.insert(
                id,
                MirBlock {
                    id: draft.id,
                    parameters: draft.parameters,
                    instructions: draft.instructions,
                    terminator,
                },
            );
        }
        finished
    }

    fn insert_value(&mut self, id: MirValueId, ty: crate::core::ResolvedTypeId, node: &NodeId) {
        if let Some(existing) = self.values.get(&id) {
            if existing.ty != ty {
                self.error(
                    node,
                    format!("value '{}' is assigned incompatible types", id),
                );
            }
            return;
        }
        self.values.insert(id.clone(), MirValue { id, ty });
    }

    fn local_value(
        &mut self,
        local: &ResolvedLocalId,
    ) -> Result<MirValueId, Vec<MirLoweringError>> {
        if let Some(value) = self.locals.get(local) {
            return Ok(value.clone());
        }
        let Some(definition) = self.body.locals.get(local) else {
            return Err(vec![MirLoweringError {
                node_id: local.0.clone(),
                message: "local is absent from ResolvedBody catalog".into(),
            }]);
        };
        let value = super::MirValueId::new(format!("local:{}", local.0 .0)).map_err(|error| {
            vec![MirLoweringError {
                node_id: local.0.clone(),
                message: error.to_string(),
            }]
        })?;
        self.insert_value(value.clone(), definition.ty.clone(), &local.0);
        self.locals.insert(local.clone(), value.clone());
        Ok(value)
    }

    fn lower_root(&mut self, root: &ResolvedBlock) {
        for statement in &root.statements {
            if self.current_is_terminated() {
                self.error(
                    &statement.node_id,
                    "statement appears after a terminating MIR instruction",
                );
                continue;
            }
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } => {
                    let value = self.lower_expr(initializer);
                    if let ResolvedPatternKind::Binding { local, .. } = &pattern.kind {
                        if let Ok(destination) = self.local_value(local) {
                            self.emit(
                                &statement.node_id,
                                "bind",
                                MirInstructionKind::Move {
                                    result: destination,
                                    source: value,
                                },
                            );
                        }
                    } else {
                        self.error(
                            &statement.node_id,
                            "only a direct binding pattern is supported in MIR Phase 0",
                        );
                    }
                }
                ResolvedStmtKind::Bind { .. } => {
                    self.error(
                        &statement.node_id,
                        "uninitialized bind is not in MIR Phase 0",
                    );
                }
                ResolvedStmtKind::Expr(expression) => {
                    if self.is_statement_if(expression) {
                        self.lower_if_stmt(&statement.node_id, expression);
                    } else {
                        let _ = self.lower_expr(expression);
                    }
                }
                ResolvedStmtKind::Return { value, .. } => {
                    let value = value.as_ref().map(|value| self.lower_return_expr(value));
                    self.terminate(MirTerminator::Return { value });
                }
                ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
                ResolvedStmtKind::While { condition, body } => {
                    self.lower_while_stmt(&statement.node_id, condition, body);
                }
                ResolvedStmtKind::Break(value) => {
                    self.lower_break(&statement.node_id, value.as_ref());
                }
                ResolvedStmtKind::Continue => {
                    self.lower_continue(&statement.node_id);
                }
                ResolvedStmtKind::Drop(places) => {
                    for (index, place) in places.iter().enumerate() {
                        if !place.projections.is_empty() {
                            self.error(
                                &statement.node_id,
                                "projected drop requires aggregate glue and remains fail-closed",
                            );
                            continue;
                        }
                        match self.local_value(&place.base) {
                            Ok(value) => self.emit(
                                &statement.node_id,
                                &format!("drop.{index}"),
                                MirInstructionKind::Drop { value },
                            ),
                            Err(errors) => self.errors.extend(errors),
                        }
                    }
                }
                _ => self.error(
                    &statement.node_id,
                    "structured control flow is not lowered by MIR Phase 0",
                ),
            }
        }
    }

    fn lower_expr(&mut self, expression: &ResolvedExpr) -> MirValueId {
        let Some(result) = self.id("expr", &expression.node_id) else {
            return self.fallback_value(expression);
        };
        self.insert_value(result.clone(), expression.ty.clone(), &expression.node_id);
        match &expression.kind {
            ResolvedExprKind::Literal(literal) => {
                self.emit(
                    &expression.node_id,
                    "const",
                    MirInstructionKind::Const {
                        result: result.clone(),
                        literal: literal.clone(),
                    },
                );
            }
            ResolvedExprKind::Constant(item) if item.0.as_str() == "builtin:value:None" => {
                let move_owned = self.type_catalog.is_some_and(|catalog| {
                    catalog.get(&expression.ty).is_some_and(|descriptor| {
                        descriptor.ownership != super::types::MirOwnership::Copy
                    })
                });
                let instruction = if move_owned {
                    MirInstructionKind::ConstructVariantMove {
                        result: result.clone(),
                        nominal: NominalTypeId::new("builtin:type:Option")
                            .expect("static Option nominal"),
                        variant: NodeId("builtin:variant:Option::None".into()),
                        fields: Vec::new(),
                    }
                } else {
                    MirInstructionKind::ConstructVariant {
                        result: result.clone(),
                        nominal: NominalTypeId::new("builtin:type:Option")
                            .expect("static Option nominal"),
                        variant: NodeId("builtin:variant:Option::None".into()),
                        fields: Vec::new(),
                    }
                };
                self.emit(&expression.node_id, "construct_variant", instruction);
            }
            ResolvedExprKind::Load(place) => {
                if let Ok(local) = self.local_value(&place.base) {
                    if matches!(
                        place.projections.as_slice(),
                        [crate::core::ir::ResolvedProjection::Deref { .. }]
                    ) {
                        self.emit(
                            &expression.node_id,
                            "project.deref",
                            MirInstructionKind::Project {
                                result: result.clone(),
                                base: local,
                                projection: super::MirProjection::Dereference,
                                list_index_contract: None,
                            },
                        );
                    } else if place.projections.is_empty() {
                        self.emit(
                            &expression.node_id,
                            "clone",
                            MirInstructionKind::Clone {
                                result: result.clone(),
                                source: local,
                            },
                        );
                    } else if let Some(projection) =
                        self.move_projection_for_place(&local, &expression.ty, place)
                    {
                        self.emit(
                            &expression.node_id,
                            "move_project",
                            MirInstructionKind::MoveProject {
                                result: result.clone(),
                                base: local,
                                projection,
                            },
                        );
                    } else if let Some(projection) =
                        self.copy_projection_for_place(&local, &expression.ty, place)
                    {
                        self.emit(
                            &expression.node_id,
                            "project",
                            MirInstructionKind::Project {
                                result: result.clone(),
                                base: local,
                                projection,
                                list_index_contract: None,
                            },
                        );
                    } else {
                        self.emit(
                            &expression.node_id,
                            "load",
                            MirInstructionKind::Load {
                                result: result.clone(),
                                place: place.clone(),
                            },
                        );
                    }
                } else {
                    self.error(
                        &expression.node_id,
                        "load base is absent from ResolvedBody local catalog",
                    );
                }
            }
            ResolvedExprKind::Unary { op, operand } => {
                let operand = self.lower_expr(operand);
                if matches!(
                    op,
                    ResolvedUnaryOp::BorrowShared | ResolvedUnaryOp::BorrowMutable
                ) {
                    self.emit(
                        &expression.node_id,
                        "borrow",
                        MirInstructionKind::Borrow {
                            result: result.clone(),
                            source: operand,
                            mutable: matches!(op, ResolvedUnaryOp::BorrowMutable),
                        },
                    );
                } else {
                    self.emit(
                        &expression.node_id,
                        "unary",
                        MirInstructionKind::Unary {
                            result: result.clone(),
                            op: *op,
                            operand,
                        },
                    );
                }
            }
            ResolvedExprKind::Binary { op, left, right } => {
                let left = self.lower_expr(left);
                let right = self.lower_expr(right);
                self.emit(
                    &expression.node_id,
                    "binary",
                    MirInstructionKind::Binary {
                        result: result.clone(),
                        op: *op,
                        left,
                        right,
                    },
                );
            }
            ResolvedExprKind::Project { value, projection } => {
                let base = self.lower_expr(value);
                let projection = match projection {
                    crate::core::ir::ResolvedValueProjection::Field(field) => {
                        super::MirProjection::Field(field.clone())
                    }
                    crate::core::ir::ResolvedValueProjection::Tuple(index) => {
                        super::MirProjection::Tuple(*index)
                    }
                    crate::core::ir::ResolvedValueProjection::Index(index) => {
                        let index = self.lower_expr(index);
                        super::MirProjection::Index(index)
                    }
                    crate::core::ir::ResolvedValueProjection::Dereference => {
                        super::MirProjection::Dereference
                    }
                };
                let list_index_contract = match &projection {
                    super::MirProjection::Index(index) => self.list_index_projection_contract(
                        &expression.node_id,
                        &base,
                        index,
                        &result,
                    ),
                    _ => None,
                };
                if self.can_move_project(&base, &result, &projection) {
                    self.emit(
                        &expression.node_id,
                        "move_project",
                        MirInstructionKind::MoveProject {
                            result: result.clone(),
                            base,
                            projection,
                        },
                    );
                } else {
                    self.emit(
                        &expression.node_id,
                        "project",
                        MirInstructionKind::Project {
                            result: result.clone(),
                            base,
                            projection,
                            list_index_contract,
                        },
                    );
                }
            }
            ResolvedExprKind::Tuple(elements) => {
                let fields = elements
                    .iter()
                    .map(|element| self.lower_expr(element))
                    .collect();
                self.emit(
                    &expression.node_id,
                    "construct",
                    MirInstructionKind::Construct {
                        result: result.clone(),
                        kind: MirAggregateKind::Tuple,
                        fields,
                    },
                );
            }
            ResolvedExprKind::List(elements) => {
                let elements = elements
                    .iter()
                    .map(|element| self.lower_expr(element))
                    .collect();
                self.emit(
                    &expression.node_id,
                    "construct_list",
                    MirInstructionKind::ConstructList {
                        result: result.clone(),
                        elements,
                    },
                );
            }
            ResolvedExprKind::Set(elements) => {
                let elements = elements
                    .iter()
                    .map(|element| self.lower_expr(element))
                    .collect();
                self.emit(
                    &expression.node_id,
                    "construct_set",
                    MirInstructionKind::ConstructSet {
                        result: result.clone(),
                        elements,
                    },
                );
            }
            ResolvedExprKind::Record {
                nominal,
                fields,
                rest,
            } => {
                let values = fields
                    .iter()
                    .map(|field| self.lower_expr(&field.value))
                    .collect();
                let kind = MirAggregateKind::Record {
                    nominal: nominal.clone(),
                    fields: fields.iter().map(|field| field.field.clone()).collect(),
                };
                if let Some(rest) = rest {
                    let base = self.lower_expr(rest);
                    self.emit(
                        &expression.node_id,
                        "update_record",
                        MirInstructionKind::UpdateRecord {
                            result: result.clone(),
                            base,
                            kind,
                            fields: values,
                        },
                    );
                } else {
                    self.emit(
                        &expression.node_id,
                        "construct",
                        MirInstructionKind::Construct {
                            result: result.clone(),
                            kind,
                            fields: values,
                        },
                    );
                }
            }
            ResolvedExprKind::Call(call) => {
                // `concat` is a destructive two-input transform.  Direct
                // local arguments therefore enter the canonical operation via
                // explicit Move values; using the ordinary Load lowering here
                // would Clone both handles and leave the source allocations
                // outside the operation's MoveOut proof.  Rvalues still use
                // their normal lowering because their fresh result is already
                // the owned operation input.
                let consuming_list_concat = is_list_concat_builtin(call, self.type_catalog);
                let arguments: Vec<MirValueId> = call
                    .arguments
                    .iter()
                    .map(|argument| {
                        if consuming_list_concat {
                            self.lower_consuming_expr(&argument.value)
                        } else {
                            self.lower_expr(&argument.value)
                        }
                    })
                    .collect();
                if let ResolvedCallee::Transition(transition) = &call.callee {
                    self.emit(
                        &expression.node_id,
                        "flow_transition",
                        MirInstructionKind::FlowTransition {
                            result: result.clone(),
                            transition: super::transition_owner_from_id(transition),
                            arguments,
                        },
                    );
                } else if let Some((nominal, variant, field_ids)) =
                    builtin_variant(call).or_else(|| user_variant(call, self.type_catalog))
                {
                    if field_ids.len() != arguments.len() {
                        self.error(
                            &expression.node_id,
                            format!(
                                "variant constructor carries {} payloads but its canonical schema expects {}",
                                arguments.len(),
                                field_ids.len()
                            ),
                        );
                    }
                    let fields = field_ids.into_iter().zip(arguments).collect();
                    let move_owned = self.type_catalog.is_some_and(|catalog| {
                        catalog.get(&expression.ty).is_some_and(|descriptor| {
                            descriptor.ownership != super::types::MirOwnership::Copy
                        })
                    });
                    let instruction = if move_owned {
                        MirInstructionKind::ConstructVariantMove {
                            result: result.clone(),
                            nominal,
                            variant,
                            fields,
                        }
                    } else {
                        MirInstructionKind::ConstructVariant {
                            result: result.clone(),
                            nominal,
                            variant,
                            fields,
                        }
                    };
                    self.emit(&expression.node_id, "construct_variant", instruction);
                } else if is_set_builtin(call, self.type_catalog) {
                    if let Some((operation, set, argument)) = set_builtin_contract(call, &arguments)
                    {
                        self.emit(
                            &expression.node_id,
                            "set_op",
                            MirInstructionKind::SetOp {
                                result: result.clone(),
                                operation,
                                set,
                                argument,
                            },
                        );
                    } else {
                        self.error(
                            &expression.node_id,
                            "Set builtin arity does not match its canonical MIR operation",
                        );
                    }
                } else if is_list_len_builtin(call, self.type_catalog) {
                    if let Some(list) = arguments.first() {
                        let list_operation_contract = self.list_operation_contract(
                            &expression.node_id,
                            &result,
                            list,
                            None,
                            super::MirListOperation::Len,
                        );
                        self.emit(
                            &expression.node_id,
                            "list_op",
                            MirInstructionKind::ListOp {
                                result: result.clone(),
                                operation: super::MirListOperation::Len,
                                list: list.clone(),
                                argument: None,
                                list_operation_contract,
                            },
                        );
                    } else {
                        self.error(
                            &expression.node_id,
                            "List.len canonical MIR operation requires one receiver",
                        );
                    }
                } else if is_list_reverse_builtin(call, self.type_catalog) {
                    if let Some(list) = arguments.first() {
                        let list_operation_contract = self.list_operation_contract(
                            &expression.node_id,
                            &result,
                            list,
                            None,
                            super::MirListOperation::Reverse,
                        );
                        self.emit(
                            &expression.node_id,
                            "list_op",
                            MirInstructionKind::ListOp {
                                result: result.clone(),
                                operation: super::MirListOperation::Reverse,
                                list: list.clone(),
                                argument: None,
                                list_operation_contract,
                            },
                        );
                    } else {
                        self.error(
                            &expression.node_id,
                            "List.reverse canonical MIR operation requires one receiver",
                        );
                    }
                } else if is_list_concat_builtin(call, self.type_catalog) {
                    if let (Some(list), Some(argument)) = (arguments.first(), arguments.get(1)) {
                        let list_operation_contract = self.list_operation_contract(
                            &expression.node_id,
                            &result,
                            list,
                            Some(argument),
                            super::MirListOperation::Concat,
                        );
                        self.emit(
                            &expression.node_id,
                            "list_op",
                            MirInstructionKind::ListOp {
                                result: result.clone(),
                                operation: super::MirListOperation::Concat,
                                list: list.clone(),
                                argument: Some(argument.clone()),
                                list_operation_contract,
                            },
                        );
                    } else {
                        self.error(
                            &expression.node_id,
                            "List.concat canonical MIR operation requires receiver and argument",
                        );
                    }
                } else if let Some(predicate) = variant_predicate_builtin(call) {
                    if let Some(variant) = arguments.first() {
                        let contract = self.variant_predicate_contract(
                            &expression.node_id,
                            &result,
                            variant,
                            predicate,
                        );
                        self.emit(
                            &expression.node_id,
                            "variant_predicate",
                            MirInstructionKind::VariantPredicate {
                                result: result.clone(),
                                predicate,
                                variant: variant.clone(),
                                contract,
                            },
                        );
                    } else {
                        self.error(
                            &expression.node_id,
                            "Option/Result variant predicate requires one receiver",
                        );
                    }
                } else if let Some(contract) = call_builtin_contract(call, self.type_catalog) {
                    self.emit(
                        &expression.node_id,
                        "builtin_call",
                        MirInstructionKind::BuiltinCall {
                            result: result.clone(),
                            kind: contract.kind,
                            arguments,
                        },
                    );
                } else {
                    let variant_call_contract = self.variant_call_abi_contract(
                        &expression.node_id,
                        &call.callee,
                        &call.type_arguments,
                        &result,
                        &arguments,
                    );
                    self.emit(
                        &expression.node_id,
                        "call",
                        MirInstructionKind::Call {
                            result: Some(result.clone()),
                            callee: call.callee.clone(),
                            type_arguments: call.type_arguments.clone(),
                            arguments,
                            variant_call_contract,
                        },
                    );
                }
            }
            ResolvedExprKind::Cast { value, .. } => {
                let source = self.lower_expr(value);
                self.emit(
                    &expression.node_id,
                    "convert",
                    MirInstructionKind::Convert {
                        result: result.clone(),
                        source,
                    },
                );
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.lower_if_expr(
                    &expression.node_id,
                    result.clone(),
                    condition,
                    then_block,
                    else_block,
                );
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.lower_match_expr(&expression.node_id, result.clone(), scrutinee, arms);
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                if let Some(value) = self.lower_block_expr(block) {
                    self.emit(
                        &expression.node_id,
                        "block",
                        MirInstructionKind::Move {
                            result: result.clone(),
                            source: value,
                        },
                    );
                }
            }
            _ => self.error(
                &expression.node_id,
                "expression shape is not lowered by MIR Phase 0",
            ),
        }
        result
    }

    fn lower_match_expr(
        &mut self,
        node: &NodeId,
        result: MirValueId,
        scrutinee: &ResolvedExpr,
        arms: &[crate::core::ir::MatchArm],
    ) {
        if arms.is_empty() {
            self.error(node, "match expression has no arms");
            return;
        }
        let consume_scrutinee = self.type_catalog.is_some_and(|catalog| {
            catalog
                .get(&scrutinee.ty)
                .is_some_and(|descriptor| descriptor.ownership != super::types::MirOwnership::Copy)
        });
        let scrutinee = if consume_scrutinee {
            self.lower_consuming_expr(scrutinee)
        } else {
            self.lower_expr(scrutinee)
        };
        let Some(scrutinee_ty) = self.values.get(&scrutinee).map(|value| value.ty.clone()) else {
            self.error(node, "match scrutinee has no canonical MIR type");
            return;
        };
        let Some(join_id) = self.block_id("match.join", node) else {
            return;
        };
        self.add_block(
            join_id.clone(),
            vec![MirBlockParameter {
                value: result.clone(),
            }],
        );

        let mut switch_arms = Vec::with_capacity(arms.len());
        let mut arm_blocks = Vec::with_capacity(arms.len());
        for arm in arms {
            let Some(case) = self.lower_switch_case(&arm.pattern.kind, &arm.node_id) else {
                continue;
            };
            if arm.guard.is_some() {
                self.error(
                    &arm.node_id,
                    "match guards require CFG lowering and are deferred to a later MIR phase",
                );
                continue;
            }
            let Some(block_id) = self.block_id("match.arm", &arm.node_id) else {
                continue;
            };
            let Some(edge) = self.edge_id("match.arm", &arm.node_id) else {
                continue;
            };
            let Some(join_edge) = self.edge_id("match.arm.join", &arm.node_id) else {
                continue;
            };
            let Some(bindings) =
                self.lower_switch_bindings(&arm.pattern, &arm.node_id, &scrutinee_ty)
            else {
                continue;
            };
            self.add_block(
                block_id.clone(),
                bindings
                    .iter()
                    .map(|binding| MirBlockParameter {
                        value: binding.parameter.clone(),
                    })
                    .collect(),
            );
            switch_arms.push(MirSwitchArm {
                edge,
                target: block_id.clone(),
                arguments: Vec::new(),
                bindings,
                case,
            });
            arm_blocks.push((block_id, join_edge, &arm.body));
        }
        if switch_arms.is_empty() {
            self.error(node, "match has no MIR-lowerable arms");
            return;
        }
        let terminator = if consume_scrutinee {
            MirTerminator::SwitchMove {
                scrutinee: scrutinee.clone(),
                arms: switch_arms,
            }
        } else {
            MirTerminator::Switch {
                scrutinee: scrutinee.clone(),
                arms: switch_arms,
            }
        };
        self.terminate(terminator);

        for (block_id, join_edge, body) in arm_blocks {
            self.switch_to(block_id);
            let value = self.lower_expr(body);
            if !self.current_is_terminated() {
                self.terminate(MirTerminator::Goto {
                    edge: join_edge,
                    target: join_id.clone(),
                    arguments: vec![value],
                });
            }
        }
        self.switch_to(join_id);
    }

    /// Lower a match scrutinee that is consumed by `SwitchMove`.  Direct local
    /// loads are moved into the terminator value so the original local is not
    /// silently cloned and left alive beside the consumed variant.  Rvalues
    /// and already ownership-aware projections keep their ordinary lowering;
    /// their result is fresh and is consumed by the terminator.
    fn lower_consuming_expr(&mut self, expression: &ResolvedExpr) -> MirValueId {
        if let ResolvedExprKind::Load(place) = &expression.kind {
            if place.projections.is_empty() {
                let Some(result) = self.id("expr", &expression.node_id) else {
                    return self.fallback_value(expression);
                };
                self.insert_value(result.clone(), expression.ty.clone(), &expression.node_id);
                match self.local_value(&place.base) {
                    Ok(source) => self.emit(
                        &expression.node_id,
                        "move",
                        MirInstructionKind::Move {
                            result: result.clone(),
                            source,
                        },
                    ),
                    Err(errors) => self.errors.extend(errors),
                }
                return result;
            }
        }
        self.lower_expr(expression)
    }

    /// Lower an expression in a return position. Direct owned `String` locals
    /// are transferred to the caller; an ordinary expression keeps its
    /// existing lowering because its result is a fresh value or belongs to a
    /// wider aggregate/control-flow contract. The TypeDesc is the only source
    /// of the ownership decision.
    fn lower_return_expr(&mut self, expression: &ResolvedExpr) -> MirValueId {
        if self
            .type_catalog
            .is_some_and(|catalog| catalog.validate_owned_string(&expression.ty).is_ok())
        {
            self.lower_consuming_expr(expression)
        } else {
            self.lower_expr(expression)
        }
    }

    fn lower_switch_bindings(
        &mut self,
        pattern: &ResolvedPattern,
        node: &NodeId,
        scrutinee_ty: &crate::core::ResolvedTypeId,
    ) -> Option<Vec<MirSwitchBinding>> {
        let ResolvedPatternKind::Constructor {
            variant, fields, ..
        } = &pattern.kind
        else {
            return Some(Vec::new());
        };
        let mut bindings = Vec::new();
        for (field, payload) in fields {
            match &payload.kind {
                ResolvedPatternKind::Wildcard => {}
                ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                } => {
                    let parameter = match self.local_value(local) {
                        Ok(value) => value,
                        Err(errors) => {
                            self.errors.extend(errors);
                            return None;
                        }
                    };
                    let Some(parameter_ty) =
                        self.values.get(&parameter).map(|value| value.ty.clone())
                    else {
                        self.error(node, "variant payload binding target has no MIR type");
                        return None;
                    };
                    let Some(type_catalog) = self.type_catalog else {
                        self.error(
                            node,
                            "variant payload binding requires a canonical TypeDesc catalog",
                        );
                        return None;
                    };
                    let projection = match type_catalog
                        .validated_variant_payload_projection_contract(
                            scrutinee_ty,
                            variant,
                            field,
                            &parameter_ty,
                        ) {
                        Ok(projection) => projection,
                        Err(message) => {
                            self.error(node, message);
                            return None;
                        }
                    };
                    bindings.push(MirSwitchBinding {
                        parameter,
                        projection,
                    });
                }
                ResolvedPatternKind::Binding { .. } => {
                    self.error(
                        node,
                        "variant payload reference bindings require ownership lowering",
                    );
                    return None;
                }
                _ => {
                    self.error(
                        node,
                        "nested variant payload patterns require destructuring MIR lowering",
                    );
                    return None;
                }
            }
        }
        Some(bindings)
    }

    fn lower_switch_case(
        &mut self,
        pattern: &ResolvedPatternKind,
        node: &NodeId,
    ) -> Option<MirSwitchCase> {
        match pattern {
            ResolvedPatternKind::Wildcard => Some(MirSwitchCase::Default),
            ResolvedPatternKind::Literal(literal) => Some(MirSwitchCase::Literal(literal.clone())),
            ResolvedPatternKind::Constructor { variant, .. } => {
                Some(MirSwitchCase::Variant(variant.clone()))
            }
            _ => {
                self.error(
                    node,
                    "tuple/array/slice patterns require destructuring MIR lowering",
                );
                None
            }
        }
    }

    fn lower_if_expr(
        &mut self,
        node: &NodeId,
        result: MirValueId,
        condition: &ResolvedExpr,
        then_block: &ResolvedBlock,
        else_block: &ResolvedBlock,
    ) {
        let condition = self.lower_expr(condition);
        let Some(then_id) = self.block_id("if.then", node) else {
            return;
        };
        let Some(else_id) = self.block_id("if.else", node) else {
            return;
        };
        let Some(join_id) = self.block_id("if.join", node) else {
            return;
        };
        let Some(then_edge) = self.edge_id("if.then", node) else {
            return;
        };
        let Some(else_edge) = self.edge_id("if.else", node) else {
            return;
        };
        let Some(then_join_edge) = self.edge_id("if.then.join", node) else {
            return;
        };
        let Some(else_join_edge) = self.edge_id("if.else.join", node) else {
            return;
        };

        self.add_block(then_id.clone(), Vec::new());
        self.add_block(else_id.clone(), Vec::new());
        self.add_block(
            join_id.clone(),
            vec![MirBlockParameter {
                value: result.clone(),
            }],
        );
        self.terminate(MirTerminator::Branch {
            condition,
            then_edge,
            then_target: then_id.clone(),
            then_arguments: Vec::new(),
            else_edge,
            else_target: else_id.clone(),
            else_arguments: Vec::new(),
        });

        self.switch_to(then_id);
        let then_value = self.lower_block_expr(then_block);
        if !self.current_is_terminated() {
            match then_value.or_else(|| self.unit_branch_value(node, &result, then_block, "then")) {
                Some(value) => self.terminate(MirTerminator::Goto {
                    edge: then_join_edge,
                    target: join_id.clone(),
                    arguments: vec![value],
                }),
                None => self.error(node, "if then branch has no value"),
            }
        }

        self.switch_to(else_id);
        let else_value = self.lower_block_expr(else_block);
        if !self.current_is_terminated() {
            match else_value.or_else(|| self.unit_branch_value(node, &result, else_block, "else")) {
                Some(value) => self.terminate(MirTerminator::Goto {
                    edge: else_join_edge,
                    target: join_id.clone(),
                    arguments: vec![value],
                }),
                None => self.error(node, "if else branch has no value"),
            }
        }
        self.switch_to(join_id);
    }

    /// Lower a statement-shaped `if` without manufacturing a Unit SSA value.
    ///
    /// Unit is a source-level result, not a native ABI value.  Keeping it out
    /// of the CFG means a branch whose body returns can join a fall-through
    /// branch using an empty edge; native and bytecode consumers therefore do
    /// not need a backend-private representation for Unit just to implement
    /// ordinary guard statements.
    fn lower_if_stmt(&mut self, node: &NodeId, expression: &ResolvedExpr) {
        let ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } = &expression.kind
        else {
            self.error(
                node,
                "statement-shaped MIR if has a non-if expression".to_string(),
            );
            return;
        };
        let condition = self.lower_expr(condition);
        let Some(then_id) = self.block_id("if.stmt.then", node) else {
            return;
        };
        let Some(else_id) = self.block_id("if.stmt.else", node) else {
            return;
        };
        let Some(join_id) = self.block_id("if.stmt.join", node) else {
            return;
        };
        let Some(then_edge) = self.edge_id("if.stmt.then", node) else {
            return;
        };
        let Some(else_edge) = self.edge_id("if.stmt.else", node) else {
            return;
        };
        let Some(then_join_edge) = self.edge_id("if.stmt.then.join", node) else {
            return;
        };
        let Some(else_join_edge) = self.edge_id("if.stmt.else.join", node) else {
            return;
        };

        self.add_block(then_id.clone(), Vec::new());
        self.add_block(else_id.clone(), Vec::new());
        self.add_block(join_id.clone(), Vec::new());
        self.terminate(MirTerminator::Branch {
            condition,
            then_edge,
            then_target: then_id.clone(),
            then_arguments: Vec::new(),
            else_edge,
            else_target: else_id.clone(),
            else_arguments: Vec::new(),
        });

        self.switch_to(then_id);
        self.lower_block_expr(then_block);
        if !self.current_is_terminated() {
            self.terminate(MirTerminator::Goto {
                edge: then_join_edge,
                target: join_id.clone(),
                arguments: Vec::new(),
            });
        }

        self.switch_to(else_id);
        self.lower_block_expr(else_block);
        if !self.current_is_terminated() {
            self.terminate(MirTerminator::Goto {
                edge: else_join_edge,
                target: join_id.clone(),
                arguments: Vec::new(),
            });
        }
        self.switch_to(join_id);
    }

    /// Lower a statement-shaped while into a canonical header/body/exit CFG.
    /// The header owns the condition so every back edge re-evaluates it; this
    /// is the key distinction from lowering a loop as a repeated AST walk.
    fn lower_while_stmt(&mut self, node: &NodeId, condition: &ResolvedExpr, body: &ResolvedBlock) {
        let Some(header_id) = self.block_id("while.header", node) else {
            return;
        };
        let Some(body_id) = self.block_id("while.body", node) else {
            return;
        };
        let Some(exit_id) = self.block_id("while.exit", node) else {
            return;
        };
        let Some(entry_edge) = self.edge_id("while.entry", node) else {
            return;
        };
        let Some(body_edge) = self.edge_id("while.body", node) else {
            return;
        };
        let Some(exit_edge) = self.edge_id("while.exit", node) else {
            return;
        };
        let Some(back_edge) = self.edge_id("while.back", node) else {
            return;
        };

        self.add_block(header_id.clone(), Vec::new());
        self.add_block(body_id.clone(), Vec::new());
        self.add_block(exit_id.clone(), Vec::new());
        self.terminate(MirTerminator::Goto {
            edge: entry_edge,
            target: header_id.clone(),
            arguments: Vec::new(),
        });

        self.switch_to(header_id.clone());
        let condition_value = self.lower_expr(condition);
        self.terminate(MirTerminator::Branch {
            condition: condition_value,
            then_edge: body_edge,
            then_target: body_id.clone(),
            then_arguments: Vec::new(),
            else_edge: exit_edge,
            else_target: exit_id.clone(),
            else_arguments: Vec::new(),
        });

        self.loops.push(LoopTargets {
            header: header_id.clone(),
            exit: exit_id.clone(),
        });
        self.switch_to(body_id);
        self.lower_block_expr(body);
        if !self.current_is_terminated() {
            self.terminate(MirTerminator::Goto {
                edge: back_edge,
                target: header_id,
                arguments: Vec::new(),
            });
        }
        self.loops.pop();
        self.switch_to(exit_id);
    }

    fn lower_break(&mut self, node: &NodeId, value: Option<&ResolvedExpr>) {
        if value.is_some() {
            self.error(node, "value-carrying break requires loop-result lowering");
            return;
        }
        let Some(exit) = self.loops.last().map(|targets| targets.exit.clone()) else {
            self.error(node, "break appears outside a loop");
            return;
        };
        let Some(edge) = self.edge_id("while.break", node) else {
            return;
        };
        self.terminate(MirTerminator::Goto {
            edge,
            target: exit,
            arguments: Vec::new(),
        });
    }

    fn lower_continue(&mut self, node: &NodeId) {
        let Some(header) = self.loops.last().map(|targets| targets.header.clone()) else {
            self.error(node, "continue appears outside a loop");
            return;
        };
        let Some(edge) = self.edge_id("while.continue", node) else {
            return;
        };
        self.terminate(MirTerminator::Goto {
            edge,
            target: header,
            arguments: Vec::new(),
        });
    }

    /// A statement-shaped `if` still has a value in canonical MIR: unit. The
    /// surface ResolvedBlock represents that as `result = None`, so materialize
    /// an explicit unit constant at each branch. We only accept it when the
    /// checker already gave the branch the same type as the enclosing result;
    /// malformed/non-unit omissions therefore remain fail-closed.
    fn unit_branch_value(
        &mut self,
        node: &NodeId,
        result: &MirValueId,
        branch: &ResolvedBlock,
        role: &str,
    ) -> Option<MirValueId> {
        let expected_ty = self.values.get(result).map(|value| value.ty.clone());
        if expected_ty.as_ref() != Some(&branch.ty) {
            self.error(node, format!("if {role} branch has no value"));
            return None;
        }
        let value = match MirValueId::new(format!("unit:{role}:{}", node.0)) {
            Ok(value) => value,
            Err(error) => {
                self.error(node, error.to_string());
                return None;
            }
        };
        self.insert_value(value.clone(), branch.ty.clone(), node);
        self.emit(
            node,
            &format!("if.{role}.unit"),
            MirInstructionKind::Const {
                result: value.clone(),
                literal: crate::core::ir::ResolvedLiteral::Unit,
            },
        );
        Some(value)
    }

    fn lower_block_expr(&mut self, block: &ResolvedBlock) -> Option<MirValueId> {
        for statement in &block.statements {
            if self.current_is_terminated() {
                self.error(
                    &statement.node_id,
                    "statement appears after a terminating MIR instruction",
                );
                continue;
            }
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } => {
                    let value = self.lower_expr(initializer);
                    if let ResolvedPatternKind::Binding { local, .. } = &pattern.kind {
                        if let Ok(destination) = self.local_value(local) {
                            self.emit(
                                &statement.node_id,
                                "bind",
                                MirInstructionKind::Move {
                                    result: destination,
                                    source: value,
                                },
                            );
                        }
                    }
                }
                ResolvedStmtKind::Expr(expression) => {
                    if self.is_statement_if(expression) {
                        self.lower_if_stmt(&statement.node_id, expression);
                    } else {
                        self.lower_expr(expression);
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    self.lower_while_stmt(&statement.node_id, condition, body);
                }
                ResolvedStmtKind::Break(value) => {
                    self.lower_break(&statement.node_id, value.as_ref());
                }
                ResolvedStmtKind::Continue => {
                    self.lower_continue(&statement.node_id);
                }
                ResolvedStmtKind::Return { value, .. } => {
                    let value = value.as_ref().map(|value| self.lower_return_expr(value));
                    self.terminate(MirTerminator::Return { value });
                }
                ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
                _ => self.error(
                    &statement.node_id,
                    "nested block statement is not lowered by MIR Phase 0",
                ),
            }
        }
        block
            .result
            .as_deref()
            .map(|result| self.lower_return_expr(result))
    }

    fn fallback_value(&mut self, expression: &ResolvedExpr) -> MirValueId {
        let value = MirValueId::new(format!("error:{}", expression.node_id.0))
            .unwrap_or_else(|_| MirValueId::new("error:fallback").expect("static MIR id"));
        self.insert_value(value.clone(), expression.ty.clone(), &expression.node_id);
        value
    }

    fn is_statement_if(&self, expression: &ResolvedExpr) -> bool {
        matches!(
            &expression.kind,
            ResolvedExprKind::If {
                then_block,
                else_block,
                ..
            } if then_block.result.is_none() && else_block.result.is_none()
        )
    }

    fn emit(&mut self, node: &NodeId, role: &str, kind: MirInstructionKind) {
        let Some(id) = self.instruction_id(node, role) else {
            return;
        };
        if let Some(block) = self.blocks.get_mut(&self.current) {
            block.instructions.push(MirInstruction { id, kind });
        } else {
            self.error(node, "cannot emit into a missing MIR block");
        }
    }

    fn can_move_project(
        &self,
        base: &MirValueId,
        result: &MirValueId,
        projection: &super::MirProjection,
    ) -> bool {
        let Some(type_catalog) = self.type_catalog else {
            return false;
        };
        let (Some(base_value), Some(result_value)) =
            (self.values.get(base), self.values.get(result))
        else {
            return false;
        };
        type_catalog
            .validate_move_projection(&base_value.ty, &result_value.ty, projection)
            .is_ok()
    }

    fn list_index_projection_contract(
        &mut self,
        node_id: &NodeId,
        base: &MirValueId,
        index: &MirValueId,
        result: &MirValueId,
    ) -> Option<super::types::MirListIndexProjectionContract> {
        let Some(type_catalog) = self.type_catalog else {
            self.error(
                node_id,
                "List index projection requires a canonical TypeDesc catalog",
            );
            return None;
        };
        let Some(base_ty) = self.values.get(base).map(|value| value.ty.clone()) else {
            self.error(node_id, "List index projection base has no MIR type");
            return None;
        };
        let Some(index_ty) = self.values.get(index).map(|value| value.ty.clone()) else {
            self.error(node_id, "List index projection operand has no MIR type");
            return None;
        };
        let Some(result_ty) = self.values.get(result).map(|value| value.ty.clone()) else {
            self.error(node_id, "List index projection result has no MIR type");
            return None;
        };
        match type_catalog.validated_list_index_projection_contract(&base_ty, &index_ty, &result_ty)
        {
            Ok(contract) => Some(contract),
            Err(message) => {
                self.error(node_id, message);
                None
            }
        }
    }

    fn list_operation_contract(
        &mut self,
        node_id: &NodeId,
        result: &MirValueId,
        list: &MirValueId,
        argument: Option<&MirValueId>,
        operation: super::MirListOperation,
    ) -> Option<super::types::MirListOperationContract> {
        let Some(type_catalog) = self.type_catalog else {
            self.error(
                node_id,
                "List operation requires a canonical TypeDesc catalog",
            );
            return None;
        };
        let Some(result_ty) = self.values.get(result).map(|value| value.ty.clone()) else {
            self.error(node_id, "List operation result has no MIR type");
            return None;
        };
        let Some(list_ty) = self.values.get(list).map(|value| value.ty.clone()) else {
            self.error(node_id, "List operation receiver has no MIR type");
            return None;
        };
        let argument_ty =
            argument.and_then(|value| self.values.get(value).map(|value| value.ty.clone()));
        match type_catalog.validated_list_operation_contract_with_argument(
            &result_ty,
            &list_ty,
            argument_ty.as_ref(),
            operation,
        ) {
            Ok(contract) => Some(contract),
            Err(message)
                if operation == super::MirListOperation::Len
                    && argument_ty.is_none()
                    && type_catalog.get(&list_ty).is_some_and(|descriptor| {
                        matches!(
                            descriptor.layout,
                            super::types::MirLayout::List { ref element }
                                if type_catalog.get(element).is_some_and(|element| {
                                    element.kind == super::types::MirTypeKind::GenericParameter
                                })
                        )
                    }) =>
            {
                match type_catalog
                    .validated_generic_list_len_operation_contract(&result_ty, &list_ty)
                {
                    Ok(contract) => Some(contract),
                    Err(generic_message) => {
                        self.error(node_id, generic_message);
                        None
                    }
                }
            }
            Err(message) => {
                self.error(node_id, message);
                None
            }
        }
    }

    fn variant_predicate_contract(
        &mut self,
        node_id: &NodeId,
        result: &MirValueId,
        variant: &MirValueId,
        predicate: MirVariantPredicate,
    ) -> Option<super::types::MirVariantPredicateContract> {
        let Some(type_catalog) = self.type_catalog else {
            self.error(
                node_id,
                "Variant predicate requires a canonical TypeDesc catalog",
            );
            return None;
        };
        let Some(result_ty) = self.values.get(result).map(|value| value.ty.clone()) else {
            self.error(node_id, "Variant predicate result has no MIR type");
            return None;
        };
        let Some(variant_ty) = self.values.get(variant).map(|value| value.ty.clone()) else {
            self.error(node_id, "Variant predicate receiver has no MIR type");
            return None;
        };
        match type_catalog.validated_variant_predicate_contract(&result_ty, &variant_ty, predicate)
        {
            Ok(contract) => Some(contract),
            Err(message) => {
                self.error(node_id, message);
                None
            }
        }
    }

    fn variant_call_abi_contract(
        &mut self,
        node_id: &NodeId,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        result: &MirValueId,
        arguments: &[MirValueId],
    ) -> Option<super::types::MirVariantCallAbiContract> {
        let Some(type_catalog) = self.type_catalog else {
            return None;
        };
        let ResolvedCallee::Function(owner) = callee else {
            return None;
        };
        let Some(result_ty) = self.values.get(result).map(|value| value.ty.clone()) else {
            self.error(node_id, "variant call result has no MIR type");
            return None;
        };
        let Some(result_desc) = type_catalog.get(&result_ty) else {
            self.error(node_id, "variant call result has no TypeDesc");
            return None;
        };
        if !matches!(
            result_desc.kind,
            super::types::MirTypeKind::Option | super::types::MirTypeKind::Result
        ) {
            return None;
        }
        let Some(parameter_types) = arguments
            .iter()
            .map(|argument| self.values.get(argument).map(|value| value.ty.clone()))
            .collect::<Option<Vec<_>>>()
        else {
            self.error(node_id, "variant call argument has no MIR type");
            return None;
        };
        let contract = if type_catalog.validate_flat_copy_variant(&result_ty).is_ok() {
            type_catalog.validated_variant_call_abi_contract(
                owner,
                type_arguments,
                &parameter_types,
                &result_ty,
            )
        } else if matches!(result_desc.kind, super::types::MirTypeKind::Result) {
            type_catalog.validated_result_string_i32_call_abi_contract(
                owner,
                type_arguments,
                &parameter_types,
                &result_ty,
            )
        } else {
            // Non-Copy Option calls remain outside this direct-call receipt
            // slice; their existing SwitchMove/clone/drop contract remains
            // valid, but a call receipt is not invented here.
            return None;
        };
        match contract {
            Ok(contract) => Some(contract),
            Err(message) => {
                self.error(node_id, message);
                None
            }
        }
    }

    fn move_projection_for_place(
        &self,
        base: &MirValueId,
        result_ty: &crate::core::ResolvedTypeId,
        place: &crate::core::ResolvedPlace,
    ) -> Option<super::MirProjection> {
        let [projection] = place.projections.as_slice() else {
            return None;
        };
        let projection = match projection {
            crate::core::ir::ResolvedProjection::Field { field, .. } => {
                super::MirProjection::Field(field.clone())
            }
            _ => return None,
        };
        let type_catalog = self.type_catalog?;
        let base_ty = self.values.get(base)?.ty.clone();
        type_catalog
            .validate_move_projection(&base_ty, result_ty, &projection)
            .is_ok()
            .then_some(projection)
    }

    fn copy_projection_for_place(
        &self,
        base: &MirValueId,
        result_ty: &crate::core::ResolvedTypeId,
        place: &crate::core::ResolvedPlace,
    ) -> Option<super::MirProjection> {
        let [projection] = place.projections.as_slice() else {
            return None;
        };
        let projection = match projection {
            crate::core::ir::ResolvedProjection::Field { field, .. } => {
                super::MirProjection::Field(field.clone())
            }
            crate::core::ir::ResolvedProjection::Tuple { index, .. } => {
                super::MirProjection::Tuple(*index)
            }
            _ => return None,
        };
        let type_catalog = self.type_catalog?;
        let base_ty = self.values.get(base)?.ty.clone();
        type_catalog
            .validate_projection(&base_ty, result_ty, &projection)
            .is_ok()
            .then_some(projection)
    }
}

fn builtin_variant(call: &ResolvedCall) -> Option<(NominalTypeId, NodeId, Vec<NodeId>)> {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    let (nominal, variant, fields) = match builtin.as_str() {
        "Some" => (
            "builtin:type:Option",
            "builtin:variant:Option::Some",
            vec![NodeId("builtin:variant:Option::Some/payload:0".into())],
        ),
        "None" => (
            "builtin:type:Option",
            "builtin:variant:Option::None",
            Vec::new(),
        ),
        "Ok" => (
            "builtin:type:Result",
            "builtin:variant:Result::Ok",
            vec![NodeId("builtin:variant:Result::Ok/payload:0".into())],
        ),
        "Err" => (
            "builtin:type:Result",
            "builtin:variant:Result::Err",
            vec![NodeId("builtin:variant:Result::Err/payload:0".into())],
        ),
        _ => return None,
    };
    Some((
        NominalTypeId::new(nominal).expect("static builtin nominal"),
        NodeId(variant.into()),
        fields,
    ))
}

/// Resolve a checker-owned user-enum constructor into the canonical variant
/// instruction.  Constructor identity and field order come only from the
/// materialized TypeDesc catalog; a constructor that is not a tagged variant
/// remains a normal call and is rejected by the canonical call graph.
fn user_variant(
    call: &ResolvedCall,
    type_catalog: Option<&MirTypeCatalog>,
) -> Option<(NominalTypeId, NodeId, Vec<NodeId>)> {
    let ResolvedCallee::Constructor(variant) = &call.callee else {
        return None;
    };
    let catalog = type_catalog?;
    let descriptor = catalog.get(&call.result)?;
    if descriptor.kind != super::types::MirTypeKind::Nominal
        || descriptor.ownership != super::types::MirOwnership::Copy
        || !matches!(descriptor.layout, super::types::MirLayout::Enum { .. })
        || catalog.validate_flat_copy_variant(&call.result).is_err()
    {
        return None;
    }
    let (nominal, variants) = catalog.variant_layout(&call.result)?;
    let descriptor = variants.iter().find(|candidate| candidate.id == *variant)?;
    Some((
        NominalTypeId::new(nominal).ok()?,
        descriptor.id.clone(),
        descriptor
            .fields
            .iter()
            .map(|field| field.id.clone())
            .collect(),
    ))
}

fn call_builtin_contract(
    call: &ResolvedCall,
    type_catalog: Option<&MirTypeCatalog>,
) -> Option<super::types::MirBuiltinContract> {
    let crate::core::ir::ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    if builtin.as_str() == "println" {
        // Keep unsupported println shapes as a canonical diagnostic witness
        // rather than a legacy surface Call. The selected integer contract
        // will be rejected by the shared TypeDesc validator when the
        // argument is not a signed scalar.
        let abi = call
            .arguments
            .first()
            .and_then(|argument| type_catalog.and_then(|catalog| catalog.get(&argument.value.ty)))
            .map(|descriptor| descriptor.abi)
            .unwrap_or(super::types::MirAbiClass::Integer {
                bits: 64,
                signed: true,
            });
        return super::types::MirBuiltinContract::from_builtin_with_abi(builtin, abi).or_else(
            || {
                Some(super::types::MirBuiltinContract::for_kind(
                    super::types::MirBuiltinKind::PrintlnInt,
                ))
            },
        );
    }
    super::types::MirBuiltinContract::from_builtin(builtin)
}

fn is_list_len_builtin(call: &ResolvedCall, type_catalog: Option<&MirTypeCatalog>) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    if !matches!(builtin.as_str(), "len" | "builtin.method.list.len") || call.arguments.len() != 1 {
        return false;
    }
    type_catalog.is_some_and(|catalog| {
        catalog
            .get(&call.arguments[0].value.ty)
            .is_some_and(|descriptor| {
                matches!(descriptor.layout, super::types::MirLayout::List { .. })
            })
    })
}

fn is_list_reverse_builtin(call: &ResolvedCall, type_catalog: Option<&MirTypeCatalog>) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    if !matches!(builtin.as_str(), "reverse" | "builtin.method.list.reverse")
        || call.arguments.len() != 1
    {
        return false;
    }
    type_catalog.is_some_and(|catalog| {
        catalog
            .get(&call.arguments[0].value.ty)
            .is_some_and(|descriptor| {
                matches!(descriptor.layout, super::types::MirLayout::List { .. })
            })
    })
}

fn is_list_concat_builtin(call: &ResolvedCall, type_catalog: Option<&MirTypeCatalog>) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    if builtin.as_str() != "builtin.method.list.concat" || call.arguments.len() != 2 {
        return false;
    }
    type_catalog.is_some_and(|catalog| {
        call.arguments.iter().all(|argument| {
            catalog.get(&argument.value.ty).is_some_and(|descriptor| {
                matches!(descriptor.layout, super::types::MirLayout::List { .. })
            })
        })
    })
}

fn variant_predicate_builtin(call: &ResolvedCall) -> Option<MirVariantPredicate> {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    if call.arguments.len() != 1 {
        return None;
    }
    match builtin.as_str() {
        "builtin.method.option.is_some" => Some(MirVariantPredicate::IsSome),
        "builtin.method.option.is_none" => Some(MirVariantPredicate::IsNone),
        "builtin.method.result.is_ok" => Some(MirVariantPredicate::IsOk),
        "builtin.method.result.is_err" => Some(MirVariantPredicate::IsErr),
        _ => None,
    }
}

fn set_builtin_contract(
    call: &ResolvedCall,
    arguments: &[MirValueId],
) -> Option<(super::MirSetOperation, MirValueId, Option<MirValueId>)> {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return None;
    };
    let operation = match builtin.as_str() {
        "builtin.method.set.size" | "builtin.method.set.len" => super::MirSetOperation::Size,
        "builtin.method.set.is_empty" => super::MirSetOperation::IsEmpty,
        "builtin.method.set.contains" | "contains" => super::MirSetOperation::Contains,
        "builtin.method.set.insert" => super::MirSetOperation::Insert,
        "builtin.method.set.remove" => super::MirSetOperation::Remove,
        // Keep this identity visible but outside this slice's contract: the
        // result needs a canonical List payload/equality/ownership contract.
        "builtin.method.set.to_list" => super::MirSetOperation::ToList,
        _ => return None,
    };
    let expected_arity = match operation {
        super::MirSetOperation::Size
        | super::MirSetOperation::IsEmpty
        | super::MirSetOperation::ToList => 1,
        super::MirSetOperation::Contains
        | super::MirSetOperation::Insert
        | super::MirSetOperation::Remove => 2,
    };
    if arguments.len() != expected_arity {
        return None;
    }
    let set = arguments.first()?.clone();
    let argument = arguments.get(1).cloned();
    Some((operation, set, argument))
}

fn is_set_builtin(call: &ResolvedCall, type_catalog: Option<&MirTypeCatalog>) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    if builtin.as_str() == "contains" {
        return call.arguments.len() == 2
            && type_catalog.is_some_and(|catalog| {
                catalog
                    .get(&call.arguments[0].value.ty)
                    .is_some_and(|descriptor| {
                        matches!(descriptor.layout, super::types::MirLayout::Set { .. })
                    })
            });
    }
    matches!(
        builtin.as_str(),
        "builtin.method.set.size"
            | "builtin.method.set.len"
            | "builtin.method.set.is_empty"
            | "builtin.method.set.contains"
            | "builtin.method.set.insert"
            | "builtin.method.set.remove"
            | "builtin.method.set.to_list"
    )
}

#[cfg(test)]
mod tests {
    use super::{lower_body, lower_body_with_type_catalog, lower_program};
    use crate::core::mir::types::MirTypeCatalog;
    use crate::core::mir::{MirInstructionKind, MirTerminator};
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    #[test]
    fn checked_scalar_function_lowers_without_backend_dependency() {
        let source = "func main() -> i32 { 40 + 2 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("MIR lowering");
        assert!(mir.validate().is_ok());
        assert!(mir.canonical_text().contains("binary"));
        let program_mir = lower_program(&program).expect("program MIR lowering");
        assert!(program_mir.contains_key(&callable.owner));
    }

    #[test]
    fn parameter_read_is_lowered_with_explicit_clone_identity() {
        let source = "func main(x: i32) -> i32 { x + 1 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("MIR lowering");
        assert!(mir.canonical_text().contains("clone"));
        assert!(mir.canonical_text().contains("local:"));
    }

    #[test]
    fn if_expression_lowers_to_branch_and_join_blocks() {
        let source = "func main() -> i32 { if true { 1 } else { 2 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("if lowering");
        assert_eq!(mir.blocks.len(), 4);
        let text = mir.canonical_text();
        assert!(text.contains("branch"));
        assert!(text.contains("bb:if.join"));
    }

    #[test]
    fn literal_match_lowers_to_switch_with_explicit_cases() {
        let source = "func main(x: i32) -> i32 { match x { 0 => 1, _ => 2 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("match lowering");
        assert_eq!(mir.blocks.len(), 4);
        let text = mir.canonical_text();
        assert!(text.contains("switch"));
        assert!(text.contains("Default"));
    }

    #[test]
    fn while_statement_lowers_to_header_body_exit_and_back_edge() {
        let source = "func main() -> i32 { while false { 1 } 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("while lowering");
        assert_eq!(mir.blocks.len(), 4);
        let text = mir.canonical_text();
        assert!(text.contains("bb:while.header"));
        assert!(text.contains("edge:while.back"));
    }

    #[test]
    fn return_inside_if_block_lowers_as_a_terminated_cfg_branch() {
        let source = "func main() -> i32 { if true { return 41 } 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("nested return lowering");
        assert!(mir
            .blocks
            .values()
            .any(|block| { matches!(block.terminator, MirTerminator::Return { value: Some(_) }) }));
        assert!(mir
            .blocks
            .values()
            .any(|block| { matches!(block.terminator, MirTerminator::Goto { .. }) }));
    }

    #[test]
    fn tuple_literal_lowers_to_an_explicit_construct_instruction() {
        let source = "func main() -> (i32, i32) { (1, 2) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("tuple lowering");
        assert!(mir.canonical_text().contains("construct"));
    }

    #[test]
    fn record_projection_and_update_lower_to_explicit_mir_nodes() {
        let source = "type Point { x: i32, y: bool }\nfunc main() -> i32 { let p = Point { x: 1, y: true }; let q = Point { y: false, ..p }; Point { x: q.x, y: false }.x }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body(&callable.body).expect("record lowering");
        let text = mir.canonical_text();
        assert!(text.contains("construct"));
        assert!(text.contains("update_record"));
        assert!(text.contains("Field(NodeId"));
    }

    #[test]
    fn option_match_lowers_to_variant_construction_and_payload_binding() {
        let source =
            "func main() -> i32 { let value: Option<i32> = Some(41); match value { Some(v) => v, None => 0 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("Option TypeDesc");
        let mir =
            lower_body_with_type_catalog(&callable.body, &catalog).expect("Option MIR lowering");
        let text = mir.canonical_text();
        assert!(text.contains("construct_variant"));
        assert!(text.contains("Variant"));
        assert!(text.contains("bind="), "{text}");
        assert!(mir.validate().is_ok(), "{:?}", mir.validate());
    }

    #[test]
    fn copy_record_field_place_load_lowers_to_canonical_project() {
        let source = "type Point { x: i32, enabled: bool }\nfunc main() -> i32 { let point = Point { x: 40, enabled: true }; if point.enabled { point.x + 2 } else { 0 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&checked).expect("TypeDesc");
        let callable = checked
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir =
            lower_body_with_type_catalog(&callable.body, &catalog).expect("record MIR lowering");
        assert!(mir
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(instruction.kind, MirInstructionKind::Project { .. })));
        assert!(!mir
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(instruction.kind, MirInstructionKind::Load { .. })));
    }

    #[test]
    fn non_copy_option_match_lowers_to_consuming_switch_move() {
        let source =
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let catalog = MirTypeCatalog::from_checked_program(&program).expect("TypeDesc");
        let callable = program
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let mir = lower_body_with_type_catalog(&callable.body, &catalog)
            .expect("consuming Option match lowering");
        let switch = mir
            .blocks
            .values()
            .find_map(|block| match &block.terminator {
                MirTerminator::SwitchMove { arms, .. } => Some(arms),
                _ => None,
            })
            .expect("non-Copy match must use SwitchMove");
        assert_eq!(switch.len(), 2);
        assert_eq!(switch[0].bindings.len(), 1);
        let projection = &switch[0].bindings[0].projection;
        assert_eq!(projection.field_index, 0);
        assert_eq!(projection.arity, 1);
        assert!(catalog.get(&projection.field_ty).is_some_and(|descriptor| {
            matches!(
                descriptor.kind,
                crate::core::mir::types::MirTypeKind::Primitive(
                    crate::core::ir::PrimitiveType::String
                )
            )
        }));
        assert!(mir
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(instruction.kind, MirInstructionKind::Move { .. })));
        assert!(mir.validate().is_ok(), "{:?}", mir.validate());
    }
}
