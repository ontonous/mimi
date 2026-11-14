//! Checker-owned declaration-time environments for direct Actor helpers.
//!
//! This receipt is deliberately separate from the scalar FFI lexical receipt.
//! A hidden suffix is admitted only for a root declaration in one selected
//! Actor method, and every use remains a direct call from that same method.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{ResolvedLocalId, ResolvedPatternKind, ResolvedStmtKind, ResolvedType};
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedCallable, ResolvedTypeId};

use super::actor::MirActorCatalog;
use super::{MirFunction, MirInstructionId, MirInstructionKind, MirValueId};

pub const MIR_ACTOR_ENVIRONMENT_SCHEMA: &str = "mimi-mir-actor-environment-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirActorEnvironmentSlot {
    pub capture: ResolvedLocalId,
    pub ty: ResolvedTypeId,
    pub declaration_copy: MirInstructionId,
    pub source: MirValueId,
    pub snapshot: MirValueId,
    pub callee_parameter: MirValueId,
    pub callee_parameter_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckedEnvironmentSnapshot {
    parent: NodeId,
    declaration: NodeId,
    callee: NodeId,
    source_parameter_count: usize,
    parameter_types: Vec<ResolvedTypeId>,
    result_type: ResolvedTypeId,
    slots: Vec<MirActorEnvironmentSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirActorEnvironmentReceipt {
    pub schema: &'static str,
    pub parent: NodeId,
    pub declaration: NodeId,
    pub callee: NodeId,
    pub source_parameter_count: usize,
    pub parameter_types: Vec<ResolvedTypeId>,
    pub result_type: ResolvedTypeId,
    pub slots: Vec<MirActorEnvironmentSlot>,
    pub call_instructions: Vec<MirInstructionId>,
    checker_snapshot: CheckedEnvironmentSnapshot,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CheckedActorEnvironmentPlan {
    snapshots: BTreeMap<NodeId, CheckedEnvironmentSnapshot>,
}

impl CheckedActorEnvironmentPlan {
    pub(crate) fn derive(
        program: &CheckedProgram,
        actors: &MirActorCatalog,
    ) -> Result<Self, Vec<String>> {
        let mut snapshots = BTreeMap::new();
        let mut errors = Vec::new();
        for (_, actor) in actors.iter() {
            for method in &actor.methods {
                // The lexical owner is the exact executable body selected by
                // the checked Actor contract. `owner` remains the source
                // mailbox identity; `body_owner` may be an explicit method or
                // the exact Flow transition body chosen by the descriptor.
                let Some(parent) = program.callable(&method.body_owner) else {
                    errors.push(format!(
                        "Actor method '{}' body owner '{}' has no checked callable",
                        method.owner.0, method.body_owner.0
                    ));
                    continue;
                };
                if parent.owner != method.body_owner {
                    errors.push(format!(
                        "Actor method '{}' body owner differs from its checked callable",
                        method.owner.0
                    ));
                    continue;
                }
                for statement in &parent.body.root.statements {
                    let ResolvedStmtKind::NestedCallable(owner) = &statement.kind else {
                        continue;
                    };
                    let result = program
                        .callable(owner)
                        .ok_or_else(|| {
                            format!("Actor nested declaration '{}' has no checked body", owner.0)
                        })
                        .and_then(|target| {
                            checked_snapshot(program, parent, target, &statement.node_id)
                        });
                    match result {
                        Ok(snapshot) => match snapshots.get(owner) {
                            Some(existing) if existing == &snapshot => (),
                            Some(_) => errors.push(format!(
                                "Actor helper '{}' has conflicting checked lexical owners",
                                owner.0
                            )),
                            None => {
                                snapshots.insert(owner.clone(), snapshot);
                            }
                        },
                        Err(message) => errors.push(message),
                    }
                }
            }
        }
        if errors.is_empty() {
            Ok(Self { snapshots })
        } else {
            Err(errors)
        }
    }

    pub(crate) fn slots(&self, owner: &NodeId) -> &[MirActorEnvironmentSlot] {
        self.snapshots
            .get(owner)
            .map(|snapshot| snapshot.slots.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn declaration(&self, parent: &NodeId, callee: &NodeId) -> Option<&NodeId> {
        self.snapshots
            .get(callee)
            .filter(|snapshot| &snapshot.parent == parent)
            .map(|snapshot| &snapshot.declaration)
    }

    pub(crate) fn target(&self, callee: &NodeId) -> Option<&NodeId> {
        self.snapshots.get(callee).map(|snapshot| &snapshot.parent)
    }

    pub(crate) fn helpers(&self) -> impl Iterator<Item = &NodeId> {
        self.snapshots.keys()
    }

    pub(crate) fn materialize(
        &self,
        functions: &BTreeMap<NodeId, MirFunction>,
    ) -> Result<BTreeMap<NodeId, MirActorEnvironmentReceipt>, Vec<String>> {
        let mut receipts = BTreeMap::new();
        let mut errors = Vec::new();
        for (owner, snapshot) in &self.snapshots {
            let mut call_instructions = Vec::new();
            for (caller, function) in functions {
                for block in function.blocks.values() {
                    for instruction in &block.instructions {
                        if matches!(&instruction.kind, MirInstructionKind::Call { callee: crate::core::ir::ResolvedCallee::Function(target), .. } if target == owner)
                        {
                            if caller != &snapshot.parent {
                                errors.push(format!(
                                    "Actor helper '{}' is called outside its declaring method",
                                    owner.0
                                ));
                            }
                            call_instructions.push(instruction.id.clone());
                        }
                    }
                }
            }
            call_instructions.sort();
            receipts.insert(
                owner.clone(),
                MirActorEnvironmentReceipt {
                    schema: MIR_ACTOR_ENVIRONMENT_SCHEMA,
                    parent: snapshot.parent.clone(),
                    declaration: snapshot.declaration.clone(),
                    callee: owner.clone(),
                    source_parameter_count: snapshot.source_parameter_count,
                    parameter_types: snapshot.parameter_types.clone(),
                    result_type: snapshot.result_type.clone(),
                    slots: snapshot.slots.clone(),
                    call_instructions,
                    checker_snapshot: snapshot.clone(),
                },
            );
        }
        if errors.is_empty() {
            validate_actor_environments(&receipts, functions)?;
            Ok(receipts)
        } else {
            Err(errors)
        }
    }
}

fn checked_snapshot(
    program: &CheckedProgram,
    parent: &ResolvedCallable,
    target: &ResolvedCallable,
    declaration: &NodeId,
) -> Result<CheckedEnvironmentSnapshot, String> {
    let error = || {
        format!(
            "Actor helper '{}' is outside the immutable i32 direct environment contract",
            target.owner.0
        )
    };
    let prefix = format!("{}/function:", parent.owner.0);
    let tail = target.owner.0.strip_prefix(&prefix).ok_or_else(error)?;
    let Some(function) = program.functions().get(&target.owner) else {
        return Err(error());
    };
    if tail.is_empty()
        || tail.contains('/')
        || target.owner != target.body.owner
        || target.owner != target.signature.owner
        || function.is_async
        || function.is_comptime
        || function.extern_abi.is_some()
        || !target.signature.generic_parameters.is_empty()
        || !target.signature.effects.is_empty()
        || !target.body.default_values.is_empty()
        || !target.contracts.is_empty()
        || target.signature.parameters.len() != target.body.parameters.len()
        || target
            .signature
            .parameters
            .iter()
            .any(|parameter| parameter.has_default || parameter.permission.is_some())
        || target
            .body
            .root
            .statements
            .iter()
            .any(|statement| matches!(statement.kind, ResolvedStmtKind::NestedCallable(_)))
    {
        return Err(error());
    }
    let is_i32 = |ty: &ResolvedTypeId| {
        matches!(
            program.resolved_types().get(ty),
            Some(ResolvedType::Primitive(PrimitiveType::I32))
        )
    };
    if !is_i32(&target.signature.result)
        || target
            .signature
            .parameters
            .iter()
            .any(|parameter| !is_i32(&parameter.ty))
    {
        return Err(error());
    }
    let mut visible = parent
        .body
        .parameters
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut found = false;
    for statement in &parent.body.root.statements {
        if statement.node_id == *declaration
            && matches!(&statement.kind, ResolvedStmtKind::NestedCallable(owner) if owner == &target.owner)
        {
            found = true;
            break;
        }
        if let ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(_),
        } = &statement.kind
        {
            if let ResolvedPatternKind::Binding { local, .. } = &pattern.kind {
                visible.insert(local.clone());
            }
        }
    }
    if !found {
        return Err(error());
    }
    let mut captures = target.body.captures.clone();
    captures.sort();
    if captures.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(error());
    }
    let mut slots = Vec::new();
    for (index, capture) in captures.iter().enumerate() {
        let source = parent.body.locals.get(capture).ok_or_else(error)?;
        let imported = target.body.locals.get(capture).ok_or_else(error)?;
        if !visible.contains(capture)
            || source.mutable
            || imported.mutable
            || source.id != *capture
            || imported.id != *capture
            || source.ty != imported.ty
            || !is_i32(&source.ty)
        {
            return Err(error());
        }
        let identity = format!("{}/capture:{}", declaration.0, capture.0 .0);
        slots.push(MirActorEnvironmentSlot {
            capture: capture.clone(),
            ty: source.ty.clone(),
            declaration_copy: MirInstructionId::new(format!("inst:actor_capture:{identity}"))
                .map_err(|e| e.to_string())?,
            source: MirValueId::new(format!("local:{}", capture.0 .0))
                .map_err(|e| e.to_string())?,
            snapshot: MirValueId::new(format!("environment:{identity}"))
                .map_err(|e| e.to_string())?,
            callee_parameter: MirValueId::new(format!("local:{}", capture.0 .0))
                .map_err(|e| e.to_string())?,
            callee_parameter_index: target
                .body
                .parameters
                .len()
                .checked_add(index)
                .ok_or_else(error)?,
        });
    }
    let parameter_types = target
        .signature
        .parameters
        .iter()
        .map(|parameter| parameter.ty.clone())
        .chain(slots.iter().map(|slot| slot.ty.clone()))
        .collect();
    Ok(CheckedEnvironmentSnapshot {
        parent: parent.owner.clone(),
        declaration: declaration.clone(),
        callee: target.owner.clone(),
        source_parameter_count: target.body.parameters.len(),
        parameter_types,
        result_type: target.signature.result.clone(),
        slots,
    })
}

pub(crate) fn validate_actor_environments(
    receipts: &BTreeMap<NodeId, MirActorEnvironmentReceipt>,
    functions: &BTreeMap<NodeId, MirFunction>,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for (key, receipt) in receipts {
        let snapshot = &receipt.checker_snapshot;
        if receipt.schema != MIR_ACTOR_ENVIRONMENT_SCHEMA
            || key != &receipt.callee
            || receipt.parent != snapshot.parent
            || receipt.declaration != snapshot.declaration
            || receipt.callee != snapshot.callee
            || receipt.source_parameter_count != snapshot.source_parameter_count
            || receipt.parameter_types != snapshot.parameter_types
            || receipt.result_type != snapshot.result_type
            || receipt.slots != snapshot.slots
        {
            errors.push(format!(
                "Actor helper '{}' environment differs from the checker lexical snapshot",
                key.0
            ));
            continue;
        }
        let Some(parent) = functions.get(&receipt.parent) else {
            errors.push(format!("Actor helper '{}' parent is absent", key.0));
            continue;
        };
        let Some(callee) = functions.get(key) else {
            errors.push(format!("Actor helper '{}' body is absent", key.0));
            continue;
        };
        let parameter_types = callee
            .parameters
            .iter()
            .filter_map(|id| callee.values.get(id).map(|value| value.ty.clone()))
            .collect::<Vec<_>>();
        if parameter_types != receipt.parameter_types
            || callee.result != receipt.result_type
            || receipt
                .source_parameter_count
                .checked_add(receipt.slots.len())
                != Some(callee.parameters.len())
        {
            errors.push(format!(
                "Actor helper '{}' hidden parameter partition is invalid",
                key.0
            ));
        }
        for slot in &receipt.slots {
            if callee.parameters.get(slot.callee_parameter_index) != Some(&slot.callee_parameter)
                || parent
                    .values
                    .get(&slot.source)
                    .is_none_or(|value| value.ty != slot.ty)
                || parent
                    .values
                    .get(&slot.snapshot)
                    .is_none_or(|value| value.ty != slot.ty)
            {
                errors.push(format!(
                    "Actor helper '{}' capture slot has a forged identity/type",
                    key.0
                ));
            }
            let copies = parent
                .blocks
                .values()
                .flat_map(|block| block.instructions.iter())
                .filter(|inst| inst.id == slot.declaration_copy)
                .collect::<Vec<_>>();
            if copies.len() != 1
                || !matches!(&copies[0].kind, MirInstructionKind::Copy { result, source } if result == &slot.snapshot && source == &slot.source)
            {
                errors.push(format!(
                    "Actor helper '{}' lacks its exact declaration-time snapshot",
                    key.0
                ));
            }
        }
        let mut calls = Vec::new();
        for (owner, function) in functions {
            for block in function.blocks.values() {
                for instruction in &block.instructions {
                    let MirInstructionKind::Call {
                        callee: crate::core::ir::ResolvedCallee::Function(target),
                        arguments,
                        type_arguments,
                        ..
                    } = &instruction.kind
                    else {
                        continue;
                    };
                    if target != key {
                        continue;
                    }
                    calls.push(instruction.id.clone());
                    let suffix = arguments.get(receipt.source_parameter_count..);
                    let expected = receipt
                        .slots
                        .iter()
                        .map(|slot| slot.snapshot.clone())
                        .collect::<Vec<_>>();
                    if owner != &receipt.parent
                        || !type_arguments.is_empty()
                        || suffix != Some(expected.as_slice())
                    {
                        errors.push(format!(
                            "Actor helper '{}' direct call/environment suffix is invalid",
                            key.0
                        ));
                    }
                }
            }
        }
        calls.sort();
        if calls != receipt.call_instructions {
            errors.push(format!(
                "Actor helper '{}' call instruction receipt is incomplete",
                key.0
            ));
        }
        if let Err(validation) = parent.validate() {
            errors.extend(validation.into_iter().map(|error| error.to_string()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

impl MirActorEnvironmentReceipt {
    pub(crate) fn canonical_text(&self) -> String {
        format!(
            "mir.actor-environment schema={} parent={} declaration={} callee={} source_parameters={} parameter_types={:?} slots={:?} calls={:?}\n",
            self.schema,
            self.parent.0,
            self.declaration.0,
            self.callee.0,
            self.source_parameter_count,
            self.parameter_types,
            self.slots,
            self.call_instructions
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mir::reference::MirProgram;

    fn program() -> MirProgram {
        let source = r#"
actor Counter {
    value: i32 = 3;
    func add(x: i32) -> i32 {
        let first = 10
        let second = 20
        func helper(y: i32) -> i32 { first + second + y }
        self.value = self.value + helper(x)
        self.value
    }
}
func main() -> i32 { let a = Counter.spawn(); println(a.add(2)); 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("Actor environment")
    }

    #[test]
    fn actor_environment_rejects_coordinated_same_type_capture_substitution() {
        let program = program();
        let mut receipts = program.actor_environments().clone();
        let mut functions = program.functions().clone();
        let receipt = receipts.values_mut().next().expect("helper receipt");
        assert_eq!(receipt.slots.len(), 2);
        let replacement = receipt.slots[1].source.clone();
        receipt.slots[0].source = replacement.clone();
        let parent = functions.get_mut(&receipt.parent).expect("method");
        let copy = parent
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| instruction.id == receipt.slots[0].declaration_copy)
            .expect("snapshot");
        if let MirInstructionKind::Copy { source, .. } = &mut copy.kind {
            *source = replacement;
        }
        assert!(validate_actor_environments(&receipts, &functions)
            .expect_err("forgery")
            .iter()
            .any(|message| message.contains("checker lexical snapshot")));
    }

    #[test]
    fn actor_environment_rejects_hidden_parameter_overflow_and_suffix_forgery() {
        let program = program();
        let mut receipts = program.actor_environments().clone();
        receipts.values_mut().next().expect("helper").slots[0].callee_parameter_index = usize::MAX;
        assert!(validate_actor_environments(&receipts, program.functions()).is_err());
        let mut functions = program.functions().clone();
        let receipt = program
            .actor_environments()
            .values()
            .next()
            .expect("helper");
        let parent = functions.get_mut(&receipt.parent).expect("method");
        let call = parent
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| receipt.call_instructions.contains(&instruction.id))
            .expect("call");
        if let MirInstructionKind::Call { arguments, .. } = &mut call.kind {
            arguments.swap(
                receipt.source_parameter_count,
                receipt.source_parameter_count + 1,
            );
        }
        assert!(
            validate_actor_environments(program.actor_environments(), &functions)
                .expect_err("suffix forgery")
                .iter()
                .any(|message| message.contains("environment suffix"))
        );
    }

    #[test]
    fn actor_environment_snapshot_must_dominate_every_direct_call() {
        let program = program();
        let mut functions = program.functions().clone();
        let receipt = program
            .actor_environments()
            .values()
            .next()
            .expect("helper");
        let parent = functions.get_mut(&receipt.parent).expect("method");
        let block = parent.blocks.get_mut(&parent.entry).expect("entry");
        let capture = block
            .instructions
            .iter()
            .position(|instruction| instruction.id == receipt.slots[0].declaration_copy)
            .expect("copy");
        let instruction = block.instructions.remove(capture);
        block.instructions.push(instruction);
        assert!(validate_actor_environments(program.actor_environments(), &functions).is_err());
    }
}
