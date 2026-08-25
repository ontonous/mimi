//! GENERIC-SHADOW-MONO-001: structural recovery of generic bindings at call
//! sites whose type arguments were never recorded (only turbofish calls land
//! in node_meta). Lives OUTSIDE codegen::resolved because the boundary lint
//! forbids AST-type dependencies there; this walks legacy `FuncDef` AST
//! signatures by design.

use crate::ast::{GenericParam, Type};
use crate::core::{ResolvedType, ResolvedTypeId, ResolvedTypeTable};
use std::collections::HashMap;

pub(super) fn infer_type_args_from_call_site(
    fdef: &crate::ast::FuncDef,
    argument_types: &[ResolvedTypeId],
    table: &ResolvedTypeTable,
    resolved_type_to_ast: impl Fn(&ResolvedType, &ResolvedTypeTable) -> Option<Type>,
) -> HashMap<String, Type> {
    fn walk(
        ast_ty: &Type,
        rid: &ResolvedTypeId,
        generic_names: &std::collections::HashSet<String>,
        table: &ResolvedTypeTable,
        binds: &mut HashMap<String, Type>,
        r2a: &impl Fn(&ResolvedType, &ResolvedTypeTable) -> Option<Type>,
    ) {
        if let Type::Name(tn, targs) = ast_ty.unlocated() {
            if targs.is_empty() && generic_names.contains(tn) {
                if let Some(rt) = table.get(rid) {
                    if let Some(concrete) = r2a(rt, table) {
                        binds.entry(tn.clone()).or_insert(concrete);
                    }
                }
                return;
            }
        }
        let (head, targs) = match ast_ty.unlocated() {
            Type::Name(head, targs) if !targs.is_empty() => (head.as_str(), targs),
            _ => return,
        };
        let Some(ResolvedType::Nominal {
            item: ritem,
            arguments: rargs,
            ..
        }) = table.get(rid)
        else {
            return;
        };
        let rhead = ritem.as_str().trim_start_matches("builtin:type:");
        if targs.len() != rargs.len()
            || !(rhead == head || rhead.ends_with(&format!("::{head}")) || head.ends_with(rhead))
        {
            return;
        }
        for (a, r) in targs.iter().zip(rargs.iter()) {
            walk(a, r, generic_names, table, binds, r2a);
        }
    }

    let generic_names: std::collections::HashSet<String> =
        fdef.generics.iter().map(|g| g.name.clone()).collect();
    let mut binds = HashMap::new();
    for (param, rid) in fdef.params.iter().zip(argument_types.iter()) {
        walk(
            &param.ty,
            rid,
            &generic_names,
            table,
            &mut binds,
            &resolved_type_to_ast,
        );
    }
    binds
}
