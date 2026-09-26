//! Checker-derived root-scope receipts for the narrow direct nested-callable
//! MIR slice. A nested declaration is compile-time scope metadata: MIR omits
//! its runtime instruction only when the checked owner identity is an
//! immediate child of a top-level user function, the helper has no closure
//! environment/default/generic ABI, directly owns a scalar extern call, and
//! every materialized use is a direct call from that declaring function.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::core::ir::{ResolvedStmtKind, ResolvedType};
use crate::core::{
    CheckedProgram, NodeId, Origin, PrimitiveType, ResolvedCallKind, ResolvedTypeId,
};
use crate::span::SourceId;

use super::reference::MirProgram;
use super::types::{MirAbiClass, MirLayout, MirOwnership, MirTypeCatalog};
use super::{MirFunction, MirInstructionId, MirInstructionKind};

pub const MIR_NESTED_CALLABLE_SCOPE_SCHEMA: &str = "mimi-mir-nested-callable-scope-v1";

/// Immutable lexical declaration witness carried with the canonical MIR
/// program. The call-site vector is materialized from the same MIR graph and
/// checked against the owner before any AST-free consumer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirNestedCallableScopeReceipt {
    pub schema: &'static str,
    pub parent: NodeId,
    pub declaration: NodeId,
    pub callee: NodeId,
    pub call_instructions: Vec<MirInstructionId>,
    pub parameter_types: Vec<ResolvedTypeId>,
    pub result_type: ResolvedTypeId,
}

impl MirNestedCallableScopeReceipt {
    pub(crate) fn canonical_text(&self) -> String {
        let mut text = String::from("mir.nested-callable-scope ");
        push_frame(&mut text, self.schema);
        push_frame(&mut text, &self.parent.0);
        push_frame(&mut text, &self.declaration.0);
        push_frame(&mut text, &self.callee.0);
        push_frame(&mut text, self.result_type.as_str());
        text.push_str("params=");
        text.push_str(&self.parameter_types.len().to_string());
        text.push('\n');
        for parameter in &self.parameter_types {
            push_frame(&mut text, parameter.as_str());
        }
        text.push_str("calls=");
        text.push_str(&self.call_instructions.len().to_string());
        text.push('\n');
        for instruction in &self.call_instructions {
            push_frame(&mut text, instruction.as_str());
        }
        text
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CheckedNestedCallableScopePlan {
    /// Parent owner → nested target owner → declaration statement identity.
    pub by_parent: BTreeMap<NodeId, BTreeMap<NodeId, NodeId>>,
    pub declarations: BTreeMap<NodeId, (NodeId, NodeId)>,
}

/// Derive only the receipt candidates whose complete lexical and signature
/// facts are present in CheckedProgram. Ineligible declarations remain
/// unapproved; the MIR lowerer will reject their declaration marker.
pub(crate) fn checked_nested_callable_scope_plan(
    program: &CheckedProgram,
    excluded_sources: Option<&HashSet<SourceId>>,
) -> CheckedNestedCallableScopePlan {
    let mut plan = CheckedNestedCallableScopePlan::default();
    for (parent_owner, parent) in program.callables() {
        if !is_top_level_function_owner(parent_owner)
            || source_is_excluded(parent, excluded_sources)
        {
            continue;
        }
        for statement in &parent.body.root.statements {
            let ResolvedStmtKind::NestedCallable(callee) = &statement.kind else {
                continue;
            };
            let Some(target) = program.callable(callee) else {
                continue;
            };
            if !is_admissible_nested_target(program, parent_owner, target, excluded_sources) {
                continue;
            }
            if !plan.declarations.contains_key(callee) {
                plan.by_parent
                    .entry(parent_owner.clone())
                    .or_default()
                    .insert(callee.clone(), statement.node_id.clone());
                plan.declarations.insert(
                    callee.clone(),
                    (parent_owner.clone(), statement.node_id.clone()),
                );
            }
        }
    }
    plan
}

fn source_is_excluded(
    callable: &crate::core::ResolvedCallable,
    excluded_sources: Option<&HashSet<SourceId>>,
) -> bool {
    excluded_sources
        .is_some_and(|excluded| excluded.contains(&callable.body.root.origin.user_span().source_id))
}

fn is_top_level_function_owner(owner: &NodeId) -> bool {
    owner.0.starts_with("function:") && !owner.0.contains("/function:") && !owner.0.contains("::")
}

fn is_admissible_nested_target(
    program: &CheckedProgram,
    parent: &NodeId,
    target: &crate::core::ResolvedCallable,
    excluded_sources: Option<&HashSet<SourceId>>,
) -> bool {
    let prefix = format!("{}/function:", parent.0);
    let Some(owner_tail) = target.owner.0.strip_prefix(&prefix) else {
        return false;
    };
    // Unit is admitted as an extern result ABI, but this receipt also
    // certifies a local MIR call result. The native emitter represents Unit
    // as LLVM void and has no BasicValue to bind; this scope receipt does not
    // yet prove that every Unit call result is unused. Keep the whole helper
    // result shape closed until that no-value use proof is part of the receipt.
    if owner_tail.is_empty()
        || owner_tail.contains('/')
        || target.owner != target.signature.owner
        || target.owner != target.body.owner
        || !target.body.captures.is_empty()
        || !target.body.default_values.is_empty()
        || !target.contracts.is_empty()
        || !target.signature.generic_parameters.is_empty()
        || target.signature.parameters.iter().any(|parameter| {
            parameter.has_default || !is_checker_scalar(program, &parameter.ty, false)
        })
        || !is_checker_scalar(program, &target.signature.result, false)
        || !program
            .call_sites()
            .values()
            .any(|site| site.owner == target.owner.0 && site.kind == ResolvedCallKind::Extern)
        || target
            .body
            .root
            .statements
            .iter()
            .any(|statement| matches!(&statement.kind, ResolvedStmtKind::NestedCallable(_)))
        || source_is_excluded(target, excluded_sources)
    {
        return false;
    }
    let Some(function) = program.functions().get(&target.owner) else {
        return false;
    };
    matches!(&function.origin, Origin::User(_))
        && !function.is_async
        && !function.is_comptime
        && function.extern_abi.is_none()
        && function.generics.is_empty()
        && function.generic_binders.is_empty()
}

fn is_checker_scalar(program: &CheckedProgram, ty: &ResolvedTypeId, allow_unit: bool) -> bool {
    match program.resolved_types().get(ty) {
        Some(ResolvedType::Primitive(
            PrimitiveType::I32
            | PrimitiveType::I64
            | PrimitiveType::Bool
            | PrimitiveType::F32
            | PrimitiveType::F64,
        )) => true,
        Some(ResolvedType::Primitive(PrimitiveType::Unit)) => allow_unit,
        _ => false,
    }
}

/// Bind checker declaration identities to every MIR direct-call instruction
/// targeting the nested helper. Calls from another owner, missing direct calls,
/// or type-argument edges remain hard validation failures.
pub(crate) fn materialize_nested_callable_scope_receipts(
    plan: &CheckedNestedCallableScopePlan,
    functions: &BTreeMap<NodeId, MirFunction>,
) -> Result<BTreeMap<NodeId, MirNestedCallableScopeReceipt>, Vec<String>> {
    let mut receipts = BTreeMap::new();
    let mut errors = Vec::new();
    for (callee, (parent, declaration)) in &plan.declarations {
        let Some(target) = functions.get(callee) else {
            errors.push(format!(
                "nested callable '{}' has no concrete MIR body",
                callee.0
            ));
            continue;
        };
        let mut call_instructions = Vec::new();
        for (caller_owner, caller) in functions {
            for block in caller.blocks.values() {
                for instruction in &block.instructions {
                    let MirInstructionKind::Call {
                        callee: call_target,
                        type_arguments,
                        ..
                    } = &instruction.kind
                    else {
                        continue;
                    };
                    if call_target == &crate::core::ir::ResolvedCallee::Function(callee.clone()) {
                        if caller_owner != parent {
                            errors.push(format!(
                                "nested callable '{}' is directly called outside declaring owner '{}'",
                                callee.0, parent.0
                            ));
                        } else if !type_arguments.is_empty() {
                            errors.push(format!(
                                "nested callable '{}' direct call carries generic type arguments",
                                callee.0
                            ));
                        } else {
                            call_instructions.push(instruction.id.clone());
                        }
                    }
                }
            }
        }
        call_instructions.sort();
        if call_instructions.is_empty() {
            errors.push(format!(
                "nested callable '{}' has no direct MIR call from declaring owner '{}'",
                callee.0, parent.0
            ));
            continue;
        }
        let mut parameter_types = Vec::with_capacity(target.parameters.len());
        for parameter in &target.parameters {
            let Some(value) = target.values.get(parameter) else {
                errors.push(format!(
                    "nested callable '{}' parameter '{}' has no canonical TypeDesc identity",
                    callee.0, parameter
                ));
                continue;
            };
            parameter_types.push(value.ty.clone());
        }
        if parameter_types.len() != target.parameters.len() {
            continue;
        }
        receipts.insert(
            callee.clone(),
            MirNestedCallableScopeReceipt {
                schema: MIR_NESTED_CALLABLE_SCOPE_SCHEMA,
                parent: parent.clone(),
                declaration: declaration.clone(),
                callee: callee.clone(),
                call_instructions,
                parameter_types,
                result_type: target.result.clone(),
            },
        );
    }
    if errors.is_empty() {
        Ok(receipts)
    } else {
        Err(errors)
    }
}

pub(crate) fn validate_nested_callable_scope_receipts(
    functions: &BTreeMap<NodeId, MirFunction>,
    type_catalog: &MirTypeCatalog,
    transitions: &BTreeMap<NodeId, super::MirTransitionContract>,
    ffi_calls: &BTreeMap<MirInstructionId, super::MirFfiCallContract>,
    receipts: &BTreeMap<NodeId, MirNestedCallableScopeReceipt>,
) -> Vec<String> {
    let mut errors = Vec::new();
    let nested_owners = functions
        .keys()
        .filter(|owner| owner.0.contains("/function:"))
        .cloned()
        .collect::<BTreeSet<_>>();
    if nested_owners != receipts.keys().cloned().collect() {
        errors.push("nested callable MIR owners and checker scope receipt targets disagree".into());
    }
    let mut declarations = BTreeSet::new();
    let mut parents = BTreeSet::new();
    for (key, receipt) in receipts {
        parents.insert(receipt.parent.clone());
        if key != &receipt.callee {
            errors.push(format!(
                "nested callable scope receipt key '{}' disagrees with target '{}'",
                key.0, receipt.callee.0
            ));
        }
        if receipt.schema != MIR_NESTED_CALLABLE_SCOPE_SCHEMA {
            errors.push(format!(
                "nested callable '{}' has unexpected scope receipt schema",
                receipt.callee.0
            ));
        }
        if receipt.declaration.0.trim().is_empty() || !declarations.insert(&receipt.declaration) {
            errors.push(format!(
                "nested callable '{}' has an empty or duplicate declaration identity",
                receipt.callee.0
            ));
        }
        let expected_prefix = format!("{}/function:", receipt.parent.0);
        if !is_top_level_function_owner(&receipt.parent)
            || !receipt.callee.0.starts_with(&expected_prefix)
            || receipt.callee.0[expected_prefix.len()..].contains('/')
        {
            errors.push(format!(
                "nested callable '{}' is not an immediate root-scope child of '{}'",
                receipt.callee.0, receipt.parent.0
            ));
        }
        let Some(target) = functions.get(&receipt.callee) else {
            errors.push(format!(
                "nested callable '{}' is absent from the canonical MIR function table",
                receipt.callee.0
            ));
            continue;
        };
        if !ffi_calls
            .values()
            .any(|ffi_call| ffi_call.caller == receipt.callee)
        {
            errors.push(format!(
                "nested callable '{}' has no validated direct scalar FFI receipt",
                receipt.callee.0
            ));
        }
        if !functions.contains_key(&receipt.parent) {
            errors.push(format!(
                "nested callable '{}' declaring owner '{}' is absent from the canonical MIR function table",
                receipt.callee.0, receipt.parent.0
            ));
        }
        let parameter_types = target
            .parameters
            .iter()
            .filter_map(|parameter| target.values.get(parameter).map(|value| value.ty.clone()))
            .collect::<Vec<_>>();
        if parameter_types != receipt.parameter_types || target.result != receipt.result_type {
            errors.push(format!(
                "nested callable '{}' MIR signature disagrees with its scope receipt",
                receipt.callee.0
            ));
        }
        for parameter in &receipt.parameter_types {
            if !is_canonical_scalar(type_catalog, parameter, false) {
                errors.push(format!(
                    "nested callable '{}' has a non-scalar parameter TypeDesc '{}'",
                    receipt.callee.0,
                    parameter.as_str()
                ));
            }
        }
        // Validate this independently from checker plan construction so a
        // forged Unit-result scope receipt cannot bypass the native no-value
        // boundary described in `is_admissible_nested_target`.
        if !is_canonical_scalar(type_catalog, &receipt.result_type, false) {
            errors.push(format!(
                "nested callable '{}' result TypeDesc '{}' is outside the native nested-call value ABI",
                receipt.callee.0,
                receipt.result_type.as_str()
            ));
        }

        let mut actual_calls = Vec::new();
        for (caller_owner, caller) in functions {
            for block in caller.blocks.values() {
                for instruction in &block.instructions {
                    let MirInstructionKind::Call {
                        callee,
                        type_arguments,
                        ..
                    } = &instruction.kind
                    else {
                        continue;
                    };
                    if callee == &crate::core::ir::ResolvedCallee::Function(receipt.callee.clone())
                    {
                        if caller_owner != &receipt.parent || !type_arguments.is_empty() {
                            errors.push(format!(
                                "nested callable '{}' has an uncovered or generic MIR call edge",
                                receipt.callee.0
                            ));
                        } else {
                            actual_calls.push(instruction.id.clone());
                        }
                    }
                }
            }
        }
        actual_calls.sort();
        if actual_calls != receipt.call_instructions
            || receipt.call_instructions.is_empty()
            || receipt
                .call_instructions
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            errors.push(format!(
                "nested callable '{}' scope receipt does not enumerate its direct call instructions",
                receipt.callee.0
            ));
        }
    }
    for parent in parents {
        if let Some(path) = recursive_call_path_from(&parent, functions, transitions) {
            errors.push(format!(
                "nested callable scope under '{}' reaches a recursive MIR call graph: {}",
                parent.0,
                path.iter()
                    .map(|owner| owner.0.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ));
        }
    }
    errors
}

fn recursive_call_path_from(
    start: &NodeId,
    functions: &BTreeMap<NodeId, MirFunction>,
    transitions: &BTreeMap<NodeId, super::MirTransitionContract>,
) -> Option<Vec<NodeId>> {
    let mut adjacency = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
    for (owner, function) in functions {
        let mut callees = BTreeSet::new();
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                match &instruction.kind {
                    MirInstructionKind::Call { callee, .. } => {
                        if let Some(target) =
                            crate::core::mir::canonical_protocol_call_target(callee)
                        {
                            if functions.contains_key(&target) {
                                callees.insert(target);
                            }
                        }
                    }
                    MirInstructionKind::FlowTransition { transition, .. } => {
                        if let Some(contract) = transitions.get(transition) {
                            if functions.contains_key(&contract.owner) {
                                callees.insert(contract.owner.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        adjacency.insert(owner.clone(), callees);
    }

    fn visit(
        owner: &NodeId,
        adjacency: &BTreeMap<NodeId, BTreeSet<NodeId>>,
        visited: &mut BTreeSet<NodeId>,
        active: &mut Vec<NodeId>,
    ) -> Option<Vec<NodeId>> {
        if let Some(cycle_start) = active.iter().position(|candidate| candidate == owner) {
            let mut cycle = active[cycle_start..].to_vec();
            cycle.push(owner.clone());
            return Some(cycle);
        }
        if !visited.insert(owner.clone()) {
            return None;
        }
        active.push(owner.clone());
        for callee in adjacency.get(owner).into_iter().flatten() {
            if let Some(cycle) = visit(callee, adjacency, visited, active) {
                return Some(cycle);
            }
        }
        active.pop();
        None
    }

    visit(start, &adjacency, &mut BTreeSet::new(), &mut Vec::new())
}

fn is_canonical_scalar(catalog: &MirTypeCatalog, ty: &ResolvedTypeId, allow_unit: bool) -> bool {
    let Some(descriptor) = catalog.get(ty) else {
        return false;
    };
    if descriptor.ownership != MirOwnership::Copy
        || descriptor.abi.canonical_ffi_scalar_kind().is_none()
    {
        return false;
    }
    match descriptor.abi {
        MirAbiClass::Unit => allow_unit && descriptor.layout == MirLayout::Unit,
        _ => descriptor.layout == MirLayout::Scalar,
    }
}

fn push_frame(output: &mut String, value: &str) {
    output.push_str(&value.len().to_string());
    output.push(':');
    output.push_str(value);
    output.push('\n');
}

pub(crate) fn canonical_nested_callable_scope_text(program: &MirProgram) -> String {
    if program.nested_callable_scopes().is_empty() {
        return String::new();
    }
    let mut text = String::from("mimi-mir-nested-callable-scope-table-v1\n");
    for receipt in program.nested_callable_scopes().values() {
        text.push_str(&receipt.canonical_text());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("nested callable tokens");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("nested callable syntax");
        crate::core::check_program(&file).expect("nested callable checked IR")
    }

    #[test]
    fn root_scalar_helper_gets_a_scope_receipt_and_shadow_uses_exact_owner() {
        let program = checked(
            r#"
                extern "C" { func labs(value: i64) -> i64; }
                func helper(value: i64) -> i64 { 999 as i64 }
                func main() -> i64 {
                    func helper(value: i64) -> i64 { labs(value) }
                    helper(-17 as i64)
                }
            "#,
        );
        let plan = checked_nested_callable_scope_plan(&program, None);
        assert_eq!(plan.declarations.len(), 1);
        let mir = MirProgram::from_checked_program(&program).expect("root-scope scalar helper MIR");
        let receipt = mir
            .nested_callable_scopes()
            .values()
            .next()
            .expect("scope receipt");
        assert_eq!(receipt.parent.0, "function:main");
        assert_eq!(receipt.call_instructions.len(), 1);
        assert!(receipt
            .callee
            .0
            .starts_with("function:main/function:helper:"));
        assert!(mir.functions().contains_key(&receipt.callee));
        assert!(mir
            .ffi_calls()
            .values()
            .any(|ffi| ffi.caller == receipt.callee));
    }

    #[test]
    fn captured_and_nonroot_nested_helpers_have_no_scope_receipt() {
        let captured = checked(
            r#"
                extern "C" { func labs(value: i64) -> i64; }
                func main(base: i64) -> i64 {
                    func helper(value: i64) -> i64 { labs(base + value) }
                    helper(1 as i64)
                }
            "#,
        );
        assert!(checked_nested_callable_scope_plan(&captured, None)
            .declarations
            .is_empty());
        let error = MirProgram::from_checked_program(&captured)
            .expect_err("captured helper has no environment ABI");
        assert!(format!("{error}").contains("lexical capture"), "{error}");

        let branch_local = checked(
            r#"
                extern "C" { func labs(value: i64) -> i64; }
                func main(flag: bool) -> i64 {
                    if flag {
                        func helper(value: i64) -> i64 { labs(value) }
                        helper(1 as i64)
                    } else {
                        0 as i64
                    }
                }
            "#,
        );
        assert!(checked_nested_callable_scope_plan(&branch_local, None)
            .declarations
            .is_empty());
        assert!(
            MirProgram::from_checked_program(&branch_local).is_err(),
            "branch-local declarations remain outside the root-scope MIR slice"
        );
    }

    #[test]
    fn recursive_nested_helper_call_graph_is_rejected_before_consumers() {
        let program = checked(
            r#"
                extern "C" { func labs(value: i64) -> i64; }
                func main() -> i32 {
                    func helper(value: i64) -> i64 { labs(main() as i64) }
                    println(helper(1 as i64))
                    0
                }
            "#,
        );
        let error = MirProgram::from_checked_program(&program)
            .expect_err("the nested helper's recursive call graph must fail closed");
        assert!(
            format!("{error}").contains("recursive MIR call graph"),
            "{error}"
        );
    }

    #[test]
    fn recursive_nested_helper_flow_transition_edges_are_rejected_before_consumers() {
        let program = checked(
            r#"
                extern "C" { func labs(value: i64) -> i64; }
                flow Counter {
                    state Zero { n: i32 }
                    transition bounce(Zero) -> Zero { return Zero { n: main() } }
                }
                func main() -> i32 {
                    func helper(value: i64) -> i64 {
                        let counter = Zero { n: 0 }
                        let returned = Counter::bounce(counter)
                        labs(value)
                    }
                    println(helper(1 as i64))
                    0
                }
            "#,
        );
        let error = MirProgram::from_checked_program(&program)
            .expect_err("FlowTransition edges must participate in recursive graph rejection");
        assert!(
            format!("{error}").contains("recursive MIR call graph"),
            "{error}"
        );
    }

    #[test]
    fn pure_root_scalar_helper_is_outside_the_ffi_scope_slice() {
        let program = checked(
            r#"
                func main() -> i64 {
                    func helper(value: i64) -> i64 { value + 1 as i64 }
                    helper(41 as i64)
                }
            "#,
        );
        assert!(checked_nested_callable_scope_plan(&program, None)
            .declarations
            .is_empty());
        let error = MirProgram::from_checked_program(&program)
            .expect_err("a pure nested helper does not gain general MIR admission");
        assert!(
            format!("{error}").contains("outside the canonical root-scope MIR slice"),
            "{error}"
        );
    }

    #[test]
    fn generated_nested_helpers_cover_each_closed_scalar_ffi_signature() {
        let cases = [
            ("i32", "i32", "i32", "i32", "1"),
            ("i64", "i64", "i64", "i64", "2 as i64"),
            ("bool", "bool", "bool", "bool", "true"),
            ("f32", "f32", "f32", "f32", "3.0 as f32"),
            ("f64", "f64", "f64", "f64", "4.0 as f64"),
            ("unit", "i32", "unit", "i32", "5"),
        ];
        let mut source = String::from("extern \"C\" {\n");
        for (name, parameter, extern_result, _, _) in cases {
            if extern_result == "unit" {
                source.push_str(&format!(
                    "    func ffi_nested_{name}(value: {parameter});\n"
                ));
            } else {
                source.push_str(&format!(
                    "    func ffi_nested_{name}(value: {parameter}) -> {extern_result};\n"
                ));
            }
        }
        source.push_str("}\nfunc main() -> i32 {\n");
        for (name, parameter, extern_result, helper_result, argument) in cases {
            if extern_result == "unit" {
                source.push_str(&format!("    func helper_{name}(value: {parameter}) -> {helper_result} {{\n        ffi_nested_{name}(value)\n        value\n    }}\n"));
            } else {
                source.push_str(&format!(
                    "    func helper_{name}(value: {parameter}) -> {helper_result} {{ ffi_nested_{name}(value) }}\n"
                ));
            }
            source.push_str(&format!(
                "    let result_{name} = helper_{name}({argument})\n"
            ));
        }
        source.push_str("    result_i32\n}\n");

        let program = checked(&source);
        let mir = MirProgram::from_checked_program(&program)
            .expect("all declared scalar ABI shapes should admit nested scope receipts");
        assert_eq!(mir.nested_callable_scopes().len(), cases.len());
        assert_eq!(mir.ffi_calls().len(), cases.len());

        for (name, parameter, extern_result, helper_result, _) in cases {
            let receipt = mir
                .nested_callable_scopes()
                .values()
                .find(|receipt| {
                    receipt
                        .callee
                        .0
                        .contains(&format!("/function:helper_{name}:"))
                })
                .expect("checker scope receipt for generated scalar signature");
            assert_eq!(receipt.parameter_types.len(), 1, "{name}");
            assert_eq!(receipt.call_instructions.len(), 1, "{name}");
            assert_eq!(
                mir.type_catalog()
                    .get(&receipt.parameter_types[0])
                    .expect("parameter TypeDesc")
                    .abi
                    .canonical_text(),
                parameter,
                "{name} parameter ABI"
            );
            assert_eq!(
                mir.type_catalog()
                    .get(&receipt.result_type)
                    .expect("result TypeDesc")
                    .abi
                    .canonical_text(),
                helper_result,
                "{name} helper result ABI"
            );
            let ffi = mir
                .ffi_calls()
                .values()
                .find(|call| call.caller == receipt.callee)
                .expect("checker-owned nested FFI receipt");
            assert_eq!(
                mir.type_catalog()
                    .get(&ffi.result_type)
                    .expect("extern result TypeDesc")
                    .abi
                    .canonical_text(),
                extern_result,
                "{name} extern result ABI"
            );
        }

        let bytecode = crate::interp::bytecode::mir::compile_mir_program(&mir)
            .expect("AST-free bytecode must preserve every generated scalar descriptor");
        assert!(bytecode.ast.is_none());
        assert_eq!(bytecode.canonical_ffi.len(), cases.len());
        assert_eq!(bytecode.canonical_ffi_bindings.len(), cases.len());

        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "nested_ffi_abi_matrix");
        codegen
            .compile_mir_native(&mir)
            .expect("native MIR must lower every generated scalar helper signature");
        codegen
            .module
            .verify()
            .expect("generated scalar nested FFI module should be valid LLVM");
    }

    #[test]
    fn unit_result_nested_helper_stays_outside_the_native_receipt_slice() {
        let program = checked(
            r#"
                extern "C" { func notify(value: i32); }
                func main() {
                    func helper(value: i32) { notify(value) }
                    let done = helper(1)
                }
            "#,
        );
        assert!(checked_nested_callable_scope_plan(&program, None)
            .declarations
            .is_empty());
        let error = MirProgram::from_checked_program(&program)
            .expect_err("unit-valued nested helper binding has no native value ABI");
        assert!(
            format!("{error}").contains("outside the canonical root-scope MIR slice"),
            "{error}"
        );
    }

    #[test]
    fn missing_and_forged_nested_scope_receipts_or_types_fail_closed() {
        let program = checked(
            r#"
                extern "C" { func labs(value: i64) -> i64; }
                func main(flag: bool) -> i64 {
                    func helper(value: i64) -> i64 { labs(value) }
                    if flag { helper(-17 as i64) } else { helper(-25 as i64) }
                }
            "#,
        );
        let mir = MirProgram::from_checked_program(&program).expect("nested scalar helper MIR");
        let receipt = mir
            .nested_callable_scopes()
            .values()
            .next()
            .expect("scope receipt")
            .clone();

        let missing = validate_nested_callable_scope_receipts(
            mir.functions(),
            mir.type_catalog(),
            mir.transitions(),
            mir.ffi_calls(),
            &BTreeMap::new(),
        );
        assert!(
            missing
                .iter()
                .any(|error| error.contains("owners and checker scope receipt targets disagree")),
            "{missing:?}"
        );

        let mut forged_schema = mir.nested_callable_scopes().clone();
        forged_schema
            .get_mut(&receipt.callee)
            .expect("nested receipt")
            .schema = "forged-schema";
        let errors = validate_nested_callable_scope_receipts(
            mir.functions(),
            mir.type_catalog(),
            mir.transitions(),
            mir.ffi_calls(),
            &forged_schema,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("unexpected scope receipt schema")),
            "{errors:?}"
        );

        let mut omitted_call = mir.nested_callable_scopes().clone();
        omitted_call
            .get_mut(&receipt.callee)
            .expect("nested receipt")
            .call_instructions
            .clear();
        let errors = validate_nested_callable_scope_receipts(
            mir.functions(),
            mir.type_catalog(),
            mir.transitions(),
            mir.ffi_calls(),
            &omitted_call,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("does not enumerate its direct call instructions")),
            "{errors:?}"
        );

        let parameter = receipt
            .parameter_types
            .first()
            .expect("scalar helper parameter")
            .clone();
        let descriptor = mir
            .type_catalog()
            .get(&parameter)
            .expect("checker-owned scalar descriptor");
        let mut forged_descriptor = descriptor.clone();
        forged_descriptor.ownership = MirOwnership::Move;
        let mut forged_catalog = mir.type_catalog().clone();
        forged_catalog.replace_for_test_only(parameter, forged_descriptor);
        let errors = validate_nested_callable_scope_receipts(
            mir.functions(),
            &forged_catalog,
            mir.transitions(),
            mir.ffi_calls(),
            mir.nested_callable_scopes(),
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("has a non-scalar parameter TypeDesc")),
            "{errors:?}"
        );
    }
}
