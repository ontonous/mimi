use crate::ast::*;
use crate::core::helpers::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    pub(crate) fn collect_decls(&mut self) {
        // Process imports: add module names to use_imports
        for import in &self.file.imports {
            if let Some(module_name) = import.path.first() {
                self.use_imports.push(module_name.clone());
            }
        }
        for item in &self.file.items {
            self.collect_item_decls(item);
        }
        // Check for type alias cycles
        self.check_alias_cycles();
    }

    /// Detect type alias cycles: type A = B; type B = A;
    pub(crate) fn check_alias_cycles(&mut self) {
        let alias_names: Vec<String> = self.aliases.keys().cloned().collect();
        for name in &alias_names {
            let mut visited = std::collections::HashSet::new();
            visited.insert(name.clone());
            if self.follows_alias_cycle(name, &visited) {
                self.emit_code(crate::diagnostic::codes::E0409, format!("type alias cycle detected: '{}' forms a cycle", name));
            }
        }
    }

    pub(crate) fn follows_alias_cycle(&self, name: &str, visited: &std::collections::HashSet<String>) -> bool {
        if let Some(Type::Name(target, _)) = self.aliases.get(name) {
            if visited.contains(target) {
                return true;
            }
            let mut new_visited = visited.clone();
            new_visited.insert(target.clone());
            return self.follows_alias_cycle(target, &new_visited);
        }
        false
    }

    pub(crate) fn collect_item_decls(&mut self, item: &Item) {
        match item {
            Item::Func(f) => {
                self.set_pos(f.pos.0, f.pos.1);
                let qualified_name = if self.module_path.is_empty() {
                    f.name.clone()
                } else {
                    format!("{}::{}", self.module_path.join("::"), f.name)
                };
                if self.funcs.contains_key(&qualified_name) {
                    self.emit_code(crate::diagnostic::codes::E0402, format!("duplicate function definition '{}'", qualified_name));
                    return;
                }
                let generic_names: Vec<String> = f.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                let params: Vec<Type> = f.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                let ret = f
                    .ret
                    .as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                let allow_passport = f.extern_abi.is_some();
                for (i, p) in f.params.iter().enumerate() {
                    if allow_passport {
                        self.check_type_well_formed_allow_passport(&params[i], &format!("parameter '{}' of function '{}'", p.name, qualified_name));
                    } else {
                        self.check_type_well_formed(&params[i], &format!("parameter '{}' of function '{}'", p.name, qualified_name));
                    }
                }
                if allow_passport {
                    self.check_type_well_formed_allow_passport(&ret, &format!("return type of function '{}'", qualified_name));
                } else {
                    self.check_type_well_formed(&ret, &format!("return type of function '{}'", qualified_name));
                }
                self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
                self.funcs.insert(qualified_name.clone(), (params, ret));
                // Store generic parameters if present
                if !f.generics.is_empty() {
                    self.func_generics.insert(qualified_name.clone(), f.generics.clone());
                }
                // Store where clause if present
                if let Some(where_clause) = &f.where_clause {
                    self.where_clauses.insert(
                        qualified_name.clone(),
                        (where_clause.type_param.clone(), where_clause.bounds.clone()),
                    );
                }
                // Store effects if present and validate against declared caps
                if !f.effects.is_empty() {
                    for effect in &f.effects {
                        if !self.declared_caps.contains(effect) {
                            self.emit_code(crate::diagnostic::codes::E0254,
                                format!("effect '{}' in function '{}' is not a declared capability. Declare it with `cap {};`",
                                    effect, f.name, effect));
                        }
                    }
                    self.func_effects.insert(qualified_name, f.effects.clone());
                }
            }
            Item::Type(t) => {
                if self.types.contains_key(&t.name) {
                    self.emit_code(crate::diagnostic::codes::E0402, format!("duplicate type definition '{}'", t.name));
                    return;
                }
                let generic_names: Vec<String> = t.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                match &t.kind {
                    TypeDefKind::Alias(ty) => {
                        let resolved = self.resolve_type(ty);
                        self.check_type_well_formed(&resolved, &format!("alias '{}'", t.name));
                        self.aliases.insert(t.name.clone(), resolved);
                    }
                    TypeDefKind::Newtype(ty) => {
                        // Store the newtype with its inner type (unresolved for now)
                        self.newtypes.insert(t.name.clone(), ty.clone());
                        // The inner type is what the constructor takes as input
                        let inner = self.resolve_type(ty);
                        self.check_type_well_formed(&inner, &format!("newtype '{}'", t.name));
                        // The return type is the newtype itself, wrapped in Type::Newtype with name
                        let self_ty = Type::Newtype(t.name.clone(), Box::new(inner.clone()));
                        self.funcs.insert(t.name.clone(), (vec![inner], self_ty));
                    }
                    TypeDefKind::Enum(variants) => {
                        let self_ty = Type::Name(t.name.clone(), vec![]);
                        for v in variants {
                            let ret = self_ty.clone();
                            let params = match &v.payload {
                                None => vec![],
                                Some(VariantPayload::Tuple(types)) => types.iter().map(|ty| self.resolve_type(ty)).collect(),
                                Some(VariantPayload::Record(fields)) => fields.iter().map(|f| self.resolve_type(&f.ty)).collect(),
                            };
                            for p in &params {
                                self.check_type_well_formed(p, &format!("variant '{}' of enum '{}'", v.name, t.name));
                            }
                            self.funcs.insert(v.name.clone(), (params, ret));
                        }
                    }
                    TypeDefKind::Record(fields) => {
                        for field in fields {
                            let field_ty = self.resolve_type(&field.ty);
                            self.check_type_well_formed(&field_ty, &format!("field '{}' of record '{}'", field.name, t.name));
                        }
                    }
                    TypeDefKind::Union(fields) => {
                        for field in fields {
                            let field_ty = self.resolve_type(&field.ty);
                            self.check_type_well_formed(&field_ty, &format!("field '{}' of union '{}'", field.name, t.name));
                        }
                    }
                }
                self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
                self.types.insert(t.name.clone(), t.clone());
                // Store generic parameters for type definitions
                if !t.generics.is_empty() {
                    self.type_generics.insert(t.name.clone(), t.generics.clone());
                }
            }
            Item::Module(m) => {
                self.module_path.push(m.name.clone());
                for inner in &m.items {
                    self.collect_item_decls(inner);
                }
                self.module_path.pop();
            }
            Item::Actor(actor) => {
                // Register actor type so it can be used as a type
                let actor_type_def = TypeDef {
                    name: actor.name.clone(),
                    pub_: actor.pub_,
                    kind: TypeDefKind::Record(actor.fields.iter().map(|f| Field {
                        name: f.name.clone(),
                        ty: f.ty.clone(),
                    }).collect()),
                    generics: Vec::new(),
                    derives: Vec::new(),
                    attributes: Vec::new(),
                };
                self.types.insert(actor.name.clone(), actor_type_def);

                // Collect actor methods as functions
                for method in &actor.methods {
                    if self.funcs.contains_key(&method.name) {
                        self.emit_code(crate::diagnostic::codes::E0402, format!("duplicate function definition '{}'", method.name));
                        return;
                    }
                    let generic_names: Vec<String> = method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(generic_names.iter().cloned());
                    // Add implicit self parameter as first param
                    let self_type = Type::Name(actor.name.clone(), vec![]);
                    let mut params = vec![self_type];
                    params.extend(method.params.iter().map(|p| self.resolve_type(&p.ty)));
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    for (i, p) in method.params.iter().enumerate() {
                        self.check_type_well_formed(&params[i + 1], &format!("parameter '{}' of actor method '{}'", p.name, method.name));
                    }
                    self.check_type_well_formed(&ret, &format!("return type of actor method '{}'", method.name));
                    self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
                    self.funcs.insert(method.name.clone(), (params, ret));
                }
            }
            Item::Cap(c) => {
                if !self.declared_caps.insert(c.name.clone()) {
                    self.emit_code(crate::diagnostic::codes::E0402,
                        format!("duplicate capability declaration '{}'", c.name));
                }
            }
            Item::Trait(trait_def) => {
                let method_names: Vec<String> = trait_def.methods.iter().map(|m| m.name.clone()).collect();
                self.traits.insert(trait_def.name.clone(), method_names.clone());
                let generic_names: Vec<String> = trait_def.generics.iter().map(|g| g.name.clone()).collect();
                self.trait_generics.insert(trait_def.name.clone(), generic_names.clone());
                // Push trait generics into scope so method signatures can reference them
                self.generic_scope.extend(generic_names.iter().cloned());
                // Store trait method signatures for argument validation
                for method in &trait_def.methods {
                    let params: Vec<Type> = method.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                    let ret = method.ret.as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    self.trait_method_sigs.insert(
                        (trait_def.name.clone(), method.name.clone()),
                        (params, ret),
                    );
                }
                self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
            }
            Item::Impl(impl_def) => {
                let method_names: Vec<String> = impl_def.methods.iter().map(|m| m.name.clone()).collect();
                self.impls.insert(
                    (impl_def.trait_name.clone(), impl_def.type_name.clone()),
                    method_names.clone(),
                );
                // Register methods available on this type via this trait
                for method_name in &method_names {
                    self.type_methods
                        .entry(impl_def.type_name.clone())
                        .or_default()
                        .push((impl_def.trait_name.clone(), method_name.clone()));
                }
                // Also register impl methods as functions with self parameter
                let impl_generic_names: Vec<String> = impl_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(impl_generic_names.iter().cloned());
                for method in &impl_def.methods {
                    let generic_names: Vec<String> = method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(generic_names.iter().cloned());
                    let mut params = vec![Type::Name(impl_def.type_name.clone(), impl_def.type_args.clone())];
                    params.extend(method.params.iter().map(|p| self.resolve_type(&p.ty)));
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    for (i, p) in method.params.iter().enumerate() {
                        self.check_type_well_formed(&params[i + 1], &format!("parameter '{}' of impl method '{}'", p.name, method.name));
                    }
                    self.check_type_well_formed(&ret, &format!("return type of impl method '{}'", method.name));
                    self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
                    let key = format!("{}_{}", impl_def.type_name, method.name);
                    self.funcs.insert(key, (params, ret));
                }
                self.generic_scope.truncate(self.generic_scope.len() - impl_generic_names.len());
            }
            Item::ExternBlock(block) => {
                // Register extern functions for type checking
                for func in &block.funcs {
                    for param in &func.params {
                        if block.unsafe_ {
                            // unsafe extern: skip passport-type validation.
                            // User takes responsibility for ABI compatibility.
                            continue;
                        }
                        let resolved = self.resolve_type(&param.ty);
                        if !self.is_valid_extern_type(&resolved, false) {
                            let type_str = fmt_type(&resolved);
                            let help = if type_str.contains("List") || type_str.starts_with('[') {
                                format!("type '{}' is a Mimi list/array and cannot cross the C ABI boundary directly. \
                                    Use a pointer (*T / *mut T) to pass array data, or serialize to JSON via the builtin JSON module.", type_str)
                            } else if type_str.contains("Option") || type_str.contains("Result") {
                                format!("type '{}' is an algebraic data type and cannot cross the C ABI boundary. \
                                    Use a plain type or a pointer (*T).", type_str)
                            } else {
                                format!("type '{}' is not allowed across the C ABI boundary. \
                                    Use scalar types (i32, i64, f64, bool, string), or *T, *mut T, c_shared T, c_borrow T, c_borrow_mut T, cap, #[repr(C)] records.", type_str)
                            };
                            self.emit_code(crate::diagnostic::codes::E0231, format!(
                                "extern function parameter '{}' has type '{}', which is not allowed to cross the C ABI boundary. {}",
                                param.name, type_str, help
                            ));
                        }
                    }
                    let params: Vec<Type> = func.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                    let ret = func.ret.as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    self.funcs.insert(func.name.clone(), (params, ret));
                }
            }
        }
    }
    pub(crate) fn check_item(&mut self, item: &Item) {
        match item {
            Item::Func(f) => {
                self.set_pos(f.pos.0, f.pos.1);
                self.check_func(f)
            }
            Item::Module(m) => {
                for inner in &m.items {
                    self.check_item(inner);
                }
            }
            Item::Actor(actor) => {
                // Check actor fields
                for field in &actor.fields {
                    let field_ty = self.resolve_type(&field.ty);
                    // Validate field type is well-formed
                    self.check_type_well_formed(&field_ty, &format!("actor field '{}'", field.name));
                    // Check field initialization if present
                    if let Some(init) = &field.init {
                        let init_ty = self.infer_expr(init, &mut vec![HashMap::new()]);
                        if !same_type(&field_ty, &init_ty) {
                            self.emit_code(crate::diagnostic::codes::E0209, format!(
                                "actor field '{}' initializer type {} does not match field type {}",
                                field.name,
                                fmt_type(&init_ty),
                                fmt_type(&field_ty)
                            ));
                        }
                    }
                }
                // Check actor methods
                for method in &actor.methods {
                    self.set_pos(method.pos.0, method.pos.1);
                    // Add implicit self parameter to scope for actor methods
                    let self_ty = Type::Name(actor.name.clone(), vec![]);
                    let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
                    scopes[0].insert("self".to_string(), self_ty);
                    // Add other params
                    for p in &method.params {
                        let ty = self.resolve_type(&p.ty);
                        scopes[0].insert(p.name.clone(), ty);
                    }
                    // Check block with self in scope
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    self.var_scopes.push(HashMap::new());
                    self.cap_vars.push(HashMap::new());
                    self.check_block(&method.body, &ret, &mut scopes);
                    self.check_unconsumed_caps();
                    self.cap_vars.pop();
                    self.var_scopes.pop();
                }
            }
            Item::Type(_) | Item::Cap(_) => {}
            Item::Trait(trait_def) => {
                // Check that all trait method types are well-formed
                let generic_names: Vec<String> = trait_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                for method in &trait_def.methods {
                    let method_generic_names: Vec<String> = method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(method_generic_names.iter().cloned());
                    for param in &method.params {
                        let resolved = self.resolve_type(&param.ty);
                        self.check_type_well_formed(&resolved, &format!("trait '{}' method '{}'", trait_def.name, method.name));
                    }
                    if let Some(ret) = &method.ret {
                        let resolved = self.resolve_type(ret);
                        self.check_type_well_formed(&resolved, &format!("trait '{}' method '{}' return", trait_def.name, method.name));
                    }
                    self.generic_scope.truncate(self.generic_scope.len() - method_generic_names.len());
                }
                self.generic_scope.truncate(self.generic_scope.len() - generic_names.len());
            }
            Item::Impl(impl_def) => {
                // Check that the trait exists
                if !self.traits.contains_key(&impl_def.trait_name) {
                    self.emit_code(crate::diagnostic::codes::E0406, format!("undefined trait '{}'", impl_def.trait_name));
                }
                // Check that the type exists
                if !self.types.contains_key(&impl_def.type_name) && !Self::is_builtin_type(&impl_def.type_name) {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0407,
                            format!("undefined type '{}'", impl_def.type_name),
                            Span::single(self.current_line, self.current_col),
                        ).with_help("types must be defined before use — check the type name spelling or add a 'type' declaration")
                    );
                }
                // Check that all required trait methods are implemented
                if let Some(required_methods) = self.traits.get(&impl_def.trait_name).cloned() {
                    let implemented: Vec<String> = impl_def.methods.iter().map(|m| m.name.clone()).collect();
                    for required in &required_methods {
                        if !implemented.contains(required) {
                            self.emit_code(crate::diagnostic::codes::E0252, format!(
                                "missing method '{}' in impl of trait '{}' for '{}'",
                                required, impl_def.trait_name, impl_def.type_name
                            ));
                        }
                    }
                }
                // Check impl method bodies with self bound to the implementing type
                let impl_generic_names: Vec<String> = impl_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(impl_generic_names.iter().cloned());
                for method in &impl_def.methods {
                    self.set_pos(method.pos.0, method.pos.1);
                    let method_generic_names: Vec<String> = method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(method_generic_names.iter().cloned());
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
                    // Bind self with the implementing type
                    scopes[0].insert("self".to_string(), Type::Name(impl_def.type_name.clone(), impl_def.type_args.clone()));
                    for p in &method.params {
                        let ty = self.resolve_type(&p.ty);
                        scopes[0].insert(p.name.clone(), ty);
                    }
                    self.var_scopes.push(HashMap::new());
                    self.cap_vars.push(HashMap::new());
                    self.check_block(&method.body, &ret, &mut scopes);
                    self.check_unconsumed_caps();
                    self.var_scopes.pop();
                    self.cap_vars.pop();
                    self.generic_scope.truncate(self.generic_scope.len() - method_generic_names.len());
                }
                self.generic_scope.truncate(self.generic_scope.len() - impl_generic_names.len());
            }
            Item::ExternBlock(_) => {
                // Extern blocks are collected but not type-checked in v1.1
            }
        }
    }
}
