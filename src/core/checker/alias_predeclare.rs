use crate::ast::{Item, Type, TypeDef, TypeDefKind};
use std::collections::{HashMap, HashSet};

use super::Checker;

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Resolved,
    Unresolvable,
}

impl<'a> Checker<'a> {
    /// Make imported public aliases available before callable signatures are
    /// collected. The file loader merges modules in deterministic path order,
    /// which can place a root function before the imported module's public
    /// alias. Only unique aliases whose targets can be resolved from builtins,
    /// already-known types, or other candidate aliases are installed here;
    /// ambiguous, cyclic, and otherwise unresolved declarations remain on the
    /// ordinary source-ordered path and keep its diagnostics.
    pub(crate) fn predeclare_imported_public_type_aliases(&mut self) {
        let mut type_name_counts = HashMap::<String, usize>::new();
        for item in &self.file.items {
            match item {
                Item::Type(definition) => {
                    *type_name_counts.entry(definition.name.clone()).or_default() += 1;
                }
                Item::Actor(actor) => {
                    *type_name_counts.entry(actor.name.clone()).or_default() += 1;
                }
                _ => {}
            }
        }

        let mut aliases = HashMap::<String, &TypeDef>::new();
        for item in &self.file.items {
            let Item::Type(definition) = item else {
                continue;
            };
            if !definition.pub_
                || type_name_counts.get(&definition.name) != Some(&1)
                || Checker::is_builtin_type(&definition.name)
                || !matches!(definition.kind, TypeDefKind::Alias(_))
                || !self.source_is_imported(definition.meta.span)
            {
                continue;
            }
            aliases.insert(definition.name.clone(), definition);
        }
        if aliases.is_empty() {
            return;
        }

        let alias_names = aliases.keys().cloned().collect::<HashSet<_>>();
        let mut dependencies = HashMap::<String, Vec<String>>::new();
        let mut resolvable = HashSet::<String>::new();
        for (name, definition) in &aliases {
            let TypeDefKind::Alias(target) = &definition.kind else {
                continue;
            };
            let generic_names = definition
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<HashSet<_>>();
            let mut referenced_names = Vec::new();
            collect_type_names(target, &mut referenced_names, &mut Vec::new());
            referenced_names.sort();
            referenced_names.dedup();

            let mut alias_dependencies = Vec::new();
            let mut target_is_resolvable = true;
            for referenced in referenced_names {
                if generic_names.contains(referenced.as_str()) {
                    continue;
                }
                if alias_names.contains(&referenced) {
                    alias_dependencies.push(referenced);
                } else if referenced == "Any"
                    || Checker::is_builtin_type(&referenced)
                    || self.types.contains_key(&referenced)
                    || self.newtypes.contains_key(&referenced)
                {
                    // This name is already available when the normal
                    // declaration pass validates the imported alias.
                } else {
                    target_is_resolvable = false;
                    break;
                }
            }
            if target_is_resolvable {
                alias_dependencies.sort();
                alias_dependencies.dedup();
                dependencies.insert(name.clone(), alias_dependencies);
                resolvable.insert(name.clone());
            }
        }

        let mut states = HashMap::<String, VisitState>::new();
        let mut names = resolvable.iter().cloned().collect::<Vec<_>>();
        names.sort();
        let mut order = Vec::new();
        for name in names {
            visit_alias(&name, &dependencies, &resolvable, &mut states, &mut order);
        }

        let previous_span = self.current_span;
        for name in order {
            let Some(definition) = aliases.get(&name).copied() else {
                continue;
            };
            let TypeDefKind::Alias(target) = &definition.kind else {
                continue;
            };
            let previous_generic_depth = self.generic_scope.len();
            self.generic_scope.extend(
                definition
                    .generics
                    .iter()
                    .map(|generic| generic.name.clone()),
            );
            if !definition.generics.is_empty() {
                self.type_generics
                    .insert(name.clone(), definition.generics.clone());
            }
            self.set_span(definition.meta.span);
            let resolved = self.resolve_type(target);
            self.aliases.insert(name.clone(), resolved);
            self.alias_spans.entry(name).or_insert(definition.meta.span);
            self.generic_scope.truncate(previous_generic_depth);
        }
        self.set_span(previous_span);
    }

    fn source_is_imported(&self, span: crate::span::Span) -> bool {
        let Some(source_key) = self.source_registry.key(span.source_id) else {
            return false;
        };
        let Some(module_path) = crate::core::module_identity::module_path_from_source_key(source_key)
        else {
            return false;
        };
        self.module_aliases
            .values()
            .any(|path| crate::core::module_identity::module_path_matches_import_path(&module_path, path))
    }
}

fn visit_alias(
    name: &str,
    dependencies: &HashMap<String, Vec<String>>,
    resolvable: &HashSet<String>,
    states: &mut HashMap<String, VisitState>,
    order: &mut Vec<String>,
) -> bool {
    match states.get(name) {
        Some(VisitState::Resolved) => return true,
        Some(VisitState::Unresolvable | VisitState::Visiting) => return false,
        None => {}
    }
    if !resolvable.contains(name) {
        states.insert(name.to_string(), VisitState::Unresolvable);
        return false;
    }

    states.insert(name.to_string(), VisitState::Visiting);
    let dependencies_resolved = dependencies
        .get(name)
        .into_iter()
        .flatten()
        .all(|dependency| visit_alias(dependency, dependencies, resolvable, states, order));
    if dependencies_resolved {
        states.insert(name.to_string(), VisitState::Resolved);
        order.push(name.to_string());
        true
    } else {
        states.insert(name.to_string(), VisitState::Unresolvable);
        false
    }
}

fn collect_type_names(ty: &Type, names: &mut Vec<String>, binders: &mut Vec<String>) {
    match ty {
        Type::Located { ty, .. } => collect_type_names(ty, names, binders),
        Type::Name(name, arguments) => {
            if !binders.iter().any(|binder| binder == name) {
                names.push(name.clone());
            }
            for argument in arguments {
                collect_type_names(argument, names, binders);
            }
        }
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Option(inner)
        | Type::Shared(inner)
        | Type::Weak(inner)
        | Type::CBuffer(inner)
        | Type::Array(inner, _)
        | Type::Slice(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::Newtype(_, inner) => collect_type_names(inner, names, binders),
        Type::Result(ok, error) => {
            collect_type_names(ok, names, binders);
            collect_type_names(error, names, binders);
        }
        Type::Tuple(elements) => {
            for element in elements {
                collect_type_names(element, names, binders);
            }
        }
        Type::Func(parameters, result) | Type::ExternFunc(parameters, result) => {
            for parameter in parameters {
                collect_type_names(parameter, names, binders);
            }
            collect_type_names(result, names, binders);
        }
        Type::ImplTrait(traits) | Type::DynTrait(traits) => names.extend(
            traits
                .iter()
                .filter(|name| !binders.iter().any(|binder| binder == *name))
                .cloned(),
        ),
        Type::ForAll(parameters, body) => {
            let previous_depth = binders.len();
            binders.extend(parameters.iter().cloned());
            collect_type_names(body, names, binders);
            binders.truncate(previous_depth);
        }
        Type::Cap(_)
        | Type::CapAtom(_)
        | Type::Nothing
        | Type::Infer
        | Type::TypeVar(_)
        | Type::TyErr => {}
    }
}
