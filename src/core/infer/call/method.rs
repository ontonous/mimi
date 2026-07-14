use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, same_type, subst_type_params, suggest_name};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_method_call(
        &mut self,
        obj: &Expr,
        method_name: &str,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // P1-16: Handle module-qualified calls via use imports.
        // merge_all flattens imported module items, so the bare function
        // name is registered in self.funcs. Route csv::parse() to
        // check_call("parse", ...) when csv is a known module name.
        if let Expr::Ident(module_name) = obj {
            if self.use_imports.contains(module_name) {
                return self.check_call(method_name, args, scopes);
            }
            // Handle flow transition call: FlowName::transition(args)
            // Prefer overload key that includes from_state of the first arg.
            let short_key = format!("flow::{}::{}", module_name, method_name);
            if self
                .funcs
                .keys()
                .any(|k| k == &short_key || k.starts_with(&format!("{}::", short_key)))
            {
                // v0.29.23: no state transition while view/mutate borrow is live.
                self.reject_transition_under_borrow(&format!(
                    "call flow transition '{}::{}'",
                    module_name, method_name
                ));
                let from_ty = if let Some(first_arg) = args.first() {
                    self.infer_expr(first_arg, scopes)
                } else {
                    Type::Name("unit".into(), vec![])
                };
                for arg in args.iter().skip(1) {
                    self.infer_expr(arg, scopes);
                }
                let overload_key = match &from_ty {
                    Type::Name(n, _) => format!("{}::{}", short_key, n),
                    _ => short_key.clone(),
                };
                if let Some((_, ret_type)) = self.funcs.get(&overload_key) {
                    return ret_type.clone();
                }
                if let Some((_, ret_type)) = self.funcs.get(&short_key) {
                    return ret_type.clone();
                }
                return Type::Name("unit".into(), vec![]);
            }
        }

        let obj_ty = self.infer_expr(obj, scopes);
        // Newtype delegates method dispatch using the newtype name.
        // e.g. UserId(42).id() looks up trait methods for "UserId".
        let (type_name, type_args): (&String, &[Type]) = match &obj_ty {
            Type::Newtype(name, _) => (name, &[]),
            Type::Name(tn, ta) => (tn, ta.as_slice()),
            _ => {
                // fall through to the rest of the method (string/list/trait check below)
                (&String::new(), &[])
            }
        };
        if !type_name.is_empty() {
            // Check built-in Option/Result methods; fall through to trait dispatch for unknown methods
            if type_name == "Option" && type_args.len() == 1 {
                let known = [
                    "unwrap",
                    "expect",
                    "unwrap_or",
                    "is_some",
                    "is_none",
                    "ok_or",
                    "map",
                    "and_then",
                    "map_err",
                ];
                if known.contains(&method_name) {
                    return self.check_option_method(method_name, &type_args[0], args, scopes);
                }
            } else if type_name == "Set" && type_args.len() == 1 {
                let known = [
                    "size", "len", "is_empty", "contains", "insert", "remove", "to_list",
                ];
                if known.contains(&method_name) {
                    return self.check_set_method(method_name, &type_args[0], args, scopes);
                }
            } else if type_name == "Result" && type_args.len() == 2 {
                let known = [
                    "unwrap",
                    "expect",
                    "unwrap_or",
                    "is_ok",
                    "is_err",
                    "map",
                    "and_then",
                    "map_err",
                    "ok_or",
                ];
                if known.contains(&method_name) {
                    return self.check_result_method(
                        method_name,
                        &type_args[0],
                        &type_args[1],
                        args,
                        scopes,
                    );
                }
            }
            // Check if it's an actor spawn call (Type.spawn)
            if method_name == "spawn" {
                return Type::Name(type_name.clone(), vec![]);
            }
            // v0.29.37: Actor.spawn_detached() — returns actor handle
            if method_name == "spawn_detached" {
                return Type::Name(type_name.clone(), vec![]);
            }
            // Check module-qualified function call: Module::func(args)
            let qualified_func = format!("{}::{}", type_name, method_name);
            if self.funcs.contains_key(&qualified_func) {
                // Determine if `qualified_func` is an actor method (registered with
                // an implicit `self` parameter by checker/items.rs:430-432). For
                // actor methods, the caller passes only the explicit args, so we
                // skip the typecheck arity check by directly inferring + returning
                // the declared return type.
                let is_actor_method = self
                    .file
                    .items
                    .iter()
                    .find_map(|item| match item {
                        Item::Actor(a) if a.name == *type_name => Some(a),
                        _ => None,
                    })
                    .map(|a| a.methods.iter().any(|m| m.name == *method_name))
                    .unwrap_or(false);
                if is_actor_method {
                    let actor = self
                        .file
                        .items
                        .iter()
                        .find_map(|item| match item {
                            Item::Actor(a) if a.name == *type_name => Some(a),
                            _ => None,
                        })
                        .expect("is_actor_method implies actor exists");
                    let method = actor
                        .methods
                        .iter()
                        .find(|m| m.name == *method_name)
                        .expect("is_actor_method implies method exists");
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    // Type-check the explicit args against declared param types.
                    if args.len() != method.params.len() {
                        self.emit_code(
                            crate::diagnostic::codes::E0257,
                            format!(
                                "method '{}' of actor '{}' expects {} arguments, got {}",
                                method_name,
                                type_name,
                                method.params.len(),
                                args.len()
                            ),
                        );
                    } else {
                        for (i, (arg, param)) in args.iter().zip(method.params.iter()).enumerate() {
                            let declared = self.resolve_type(&param.ty);
                            let at = self.infer_expr(arg, scopes);
                            // IF-C4: unify so TypeVars / Option payloads resolve.
                            if self.unification.unify(&at, &declared).is_err() {
                                self.emit_code(
                                    crate::diagnostic::codes::E0211,
                                    format!(
                                        "argument {} of method '{}' expected {}, found {}",
                                        i + 1,
                                        method_name,
                                        fmt_type(&declared),
                                        fmt_type(&at)
                                    ),
                                );
                            }
                            let _ = i;
                        }
                    }
                    return ret;
                }
                return self.check_call(&qualified_func, args, scopes);
            }
            // Check record field access (field is a closure/function)
            if let Some(tdef) = self.types.get(type_name) {
                if let TypeDefKind::Record(fields) = &tdef.kind {
                    if let Some(f) = fields.iter().find(|f| f.name == method_name) {
                        return self.resolve_type(&f.ty);
                    }
                }
                if let TypeDefKind::Enum(variants) = &tdef.kind {
                    if variants.iter().any(|v| v.name == method_name) {
                        return Type::Name(type_name.clone(), vec![]);
                    }
                }
            }
            // Check trait methods on this type
            if let Some(methods) = self.type_methods.get(type_name) {
                if let Some((trait_name, _)) = methods.iter().find(|(_, m)| m == method_name) {
                    let trait_name = trait_name.clone();
                    if let Some((params, ret)) = self
                        .trait_method_sigs
                        .get(&(trait_name.clone(), method_name.to_string()))
                        .cloned()
                    {
                        let (method_params, method_ret) = if let Some(trait_generic_names) =
                            self.trait_generics.get(&trait_name)
                        {
                            if !trait_generic_names.is_empty()
                                && trait_generic_names.len() == type_args.len()
                            {
                                let type_map: HashMap<String, Type> = trait_generic_names
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(g, a)| (g.clone(), a.clone()))
                                    .collect();
                                let gen_slice: Vec<GenericParam> = trait_generic_names
                                    .iter()
                                    .map(|g| GenericParam {
                                        name: g.clone(),
                                        bounds: vec![],
                                    })
                                    .collect();
                                let subst_params: Vec<Type> = params
                                    .iter()
                                    .map(|p| subst_type_params(p, &gen_slice, &type_map))
                                    .collect();
                                let subst_ret = subst_type_params(&ret, &gen_slice, &type_map);
                                (subst_params, subst_ret)
                            } else {
                                (params, ret)
                            }
                        } else {
                            (params, ret)
                        };
                        let user_args = &args;
                        if user_args.len() != method_params.len() {
                            self.emit_code(
                                crate::diagnostic::codes::E0257,
                                format!(
                                    "method '{}' of trait '{}' expects {} arguments, got {}",
                                    method_name,
                                    trait_name,
                                    method_params.len(),
                                    user_args.len()
                                ),
                            );
                        } else {
                            for (i, (arg, param)) in
                                user_args.iter().zip(method_params.iter()).enumerate()
                            {
                                let at = self.infer_expr(arg, scopes);
                                // IF-C5: unify so TypeVars / Option payloads resolve.
                                if self.unification.unify(&at, param).is_err() {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0211,
                                        format!(
                                            "argument {} of method '{}' expected {}, found {}",
                                            i + 1,
                                            method_name,
                                            fmt_type(param),
                                            fmt_type(&at)
                                        ),
                                    );
                                }
                            }
                        }
                        return method_ret;
                    }
                }
            }
            // Check if the type has this as a direct method (actor methods)
            if let Some(actor_def) = self.file.items.iter().find_map(|item| {
                if let Item::Actor(a) = item {
                    if a.name == *type_name {
                        Some(a)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }) {
                if let Some(method) = actor_def.methods.iter().find(|m| m.name == *method_name) {
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    return ret;
                }
            }
            // Check string methods
            if type_name == "string" {
                return self.check_string_method(method_name, args, scopes);
            }
            // Check list methods
            if type_name == "List" && method_name == "len" {
                return self.check_list_method(method_name, args, scopes);
            }
            let mut method_candidates: Vec<String> = self
                .type_methods
                .get(type_name)
                .map(|methods| methods.iter().map(|(_, m)| m.clone()).collect())
                .unwrap_or_default();
            if let Some(actor_def) = self.file.items.iter().find_map(|item| {
                if let Item::Actor(a) = item {
                    if a.name == *type_name {
                        Some(a)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }) {
                method_candidates.extend(actor_def.methods.iter().map(|m| m.name.clone()));
            }
            let suggestion = suggest_name(method_name, &method_candidates, 3);
            let help = if let Some(s) = suggestion {
                format!("did you mean '{}'?", s)
            } else {
                "check the method name spelling or available methods for this type".to_string()
            };
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0221,
                    format!("type '{}' has no method '{}'", type_name, method_name),
                    Span::single(self.current_line, self.current_col),
                )
                .with_help(&help),
            );
            Type::Name("unknown".into(), vec![])
        } else if let Type::DynTrait(traits) = &obj_ty {
            self.resolve_trait_method(traits, method_name, args, scopes)
        } else if let Type::ImplTrait(traits) = &obj_ty {
            self.resolve_trait_method(traits, method_name, args, scopes)
        } else if let Type::Option(inner) = &obj_ty {
            // Codegen supports `.deref()` on `Option<shared T>` / `Option<local_shared T>`
            // (produced by `weak.upgrade()`), where deref extracts the shared payload.
            if method_name == "deref"
                && matches!(inner.as_ref(), Type::Shared(_) | Type::LocalShared(_))
            {
                match inner.as_ref() {
                    Type::Shared(i) | Type::LocalShared(i) => (**i).clone(),
                    _ => Type::Name("unknown".into(), vec![]),
                }
            } else {
                self.check_option_method(method_name, inner, args, scopes)
            }
        } else if let Type::Result(ok_ty, err_ty) = &obj_ty {
            self.check_result_method(method_name, ok_ty, err_ty, args, scopes)
        } else if let Type::Shared(inner) = &obj_ty {
            self.check_shared_method(method_name, inner)
        } else if let Type::LocalShared(inner) = &obj_ty {
            self.check_local_shared_method(method_name, inner)
        } else if let Type::Weak(inner) = &obj_ty {
            self.check_weak_method(method_name, inner)
        } else if let Type::WeakLocal(inner) = &obj_ty {
            self.check_weak_local_method(method_name, inner)
        } else {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0222,
                    format!(
                        "method call requires a named type, found {}",
                        fmt_type(&obj_ty)
                    ),
                    Span::single(self.current_line, self.current_col),
                )
                .with_help("only named types (record, enum, actor) have methods"),
            );
            Type::Name("unknown".into(), vec![])
        }
    }

    pub(in crate::core) fn resolve_trait_method(
        &mut self,
        traits: &[String],
        method_name: &str,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        for trait_name in traits {
            if let Some((params, ret)) = self
                .trait_method_sigs
                .get(&(trait_name.clone(), method_name.to_string()))
                .cloned()
            {
                let user_args = &args;
                let method_params = &params;
                if user_args.len() != method_params.len() {
                    self.emit_code(
                        crate::diagnostic::codes::E0257,
                        format!(
                            "method '{}' of trait '{}' expects {} arguments, got {}",
                            method_name,
                            trait_name,
                            method_params.len(),
                            user_args.len()
                        ),
                    );
                } else {
                    for (i, (arg, param)) in user_args.iter().zip(method_params.iter()).enumerate()
                    {
                        let at = self.infer_expr(arg, scopes);
                        if !same_type(&at, param) {
                            self.emit_code(
                                crate::diagnostic::codes::E0211,
                                format!(
                                    "argument {} of method '{}' expected {}, found {}",
                                    i + 1,
                                    method_name,
                                    fmt_type(param),
                                    fmt_type(&at)
                                ),
                            );
                        }
                    }
                }
                return ret;
            }
        }
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0221,
                format!("trait object does not have method '{}'", method_name),
                Span::single(self.current_line, self.current_col),
            )
            .with_help("check the method name spelling or available methods for this type"),
        );
        Type::Name("unknown".into(), vec![])
    }

    pub(in crate::core) fn check_shared_method(&mut self, method: &str, inner: &Type) -> Type {
        match method {
            "clone" => Type::Shared(Box::new(inner.clone())),
            "deref" | "inner" => inner.clone(),
            _ => {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0221,
                        format!(
                            "type 'shared {}' has no method '{}'",
                            fmt_type(inner),
                            method
                        ),
                        Span::single(self.current_line, self.current_col),
                    )
                    .with_help("shared values support clone, deref, inner"),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn check_local_shared_method(
        &mut self,
        method: &str,
        inner: &Type,
    ) -> Type {
        match method {
            "clone" => Type::LocalShared(Box::new(inner.clone())),
            "deref" | "inner" => inner.clone(),
            _ => {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0221,
                        format!(
                            "type 'local_shared {}' has no method '{}'",
                            fmt_type(inner),
                            method
                        ),
                        Span::single(self.current_line, self.current_col),
                    )
                    .with_help("local_shared values support clone, deref, inner"),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn check_weak_method(&mut self, method: &str, inner: &Type) -> Type {
        match method {
            "upgrade" => Type::Option(Box::new(Type::Shared(Box::new(inner.clone())))),
            _ => {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0221,
                        format!("type 'weak {}' has no method '{}'", fmt_type(inner), method),
                        Span::single(self.current_line, self.current_col),
                    )
                    .with_help("weak values support upgrade"),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn check_weak_local_method(&mut self, method: &str, inner: &Type) -> Type {
        match method {
            "upgrade" => Type::Option(Box::new(Type::LocalShared(Box::new(inner.clone())))),
            _ => {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0221,
                        format!(
                            "type 'weak_local {}' has no method '{}'",
                            fmt_type(inner),
                            method
                        ),
                        Span::single(self.current_line, self.current_col),
                    )
                    .with_help("weak_local values support upgrade"),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn infer_turbofish(
        &mut self,
        name: &str,
        type_args: &[Type],
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // Special case: from_json::<T>(s) — typed JSON deserialization
        if name == "from_json" && !type_args.is_empty() {
            if type_args.len() != 1 {
                self.emit_code(
                    crate::diagnostic::codes::E0239,
                    "from_json expects at most 1 type argument",
                );
                return Type::Name("unknown".into(), vec![]);
            }
            if args.len() != 1 {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    "from_json::<T> expects 1 argument (json string)",
                );
            } else {
                self.infer_expr(&args[0], scopes);
            }
            return type_args[0].clone();
        }
        // Turbofish: func::<Type>(args) — explicit type instantiation
        let (params, ret) = match self.funcs.get(name) {
            Some(sig) => sig.clone(),
            None => {
                self.emit_code(
                    crate::diagnostic::codes::E0401,
                    format!("undefined function '{}'", name),
                );
                return Type::Name("unknown".into(), vec![]);
            }
        };
        let generics = self.func_generics.get(name).cloned().unwrap_or_default();

        // Build type param map from turbofish type args
        let mut type_map: HashMap<String, Type> = HashMap::new();
        if !generics.is_empty() && !type_args.is_empty() {
            if type_args.len() != generics.len() {
                self.emit_code(
                    crate::diagnostic::codes::E0239,
                    format!(
                        "function '{}' expects {} type arguments, got {}",
                        name,
                        generics.len(),
                        type_args.len()
                    ),
                );
            } else {
                for (gp, ta) in generics.iter().zip(type_args.iter()) {
                    type_map.insert(gp.name.clone(), ta.clone());
                }
            }
        }

        if args.len() != params.len() {
            self.emit_code(
                crate::diagnostic::codes::E0257,
                format!(
                    "function '{}' expects {} arguments, got {}",
                    name,
                    params.len(),
                    args.len()
                ),
            );
        } else {
            // Check where constraints (before substitution)
            if let Some((type_param, bounds)) = self.where_clauses.get(name).cloned() {
                for (arg, param) in args.iter().zip(params.iter()) {
                    let at = self.infer_expr(arg, scopes);
                    if self.type_uses_type_param(param, &type_param) {
                        for bound in &bounds {
                            if !self.type_implements_trait(&at, bound) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0253,
                                    format!(
                                        "where constraint violated: type '{}' does not implement trait '{}' (required by function '{}')",
                                        fmt_type(&at),
                                        bound,
                                        name
                                    ),
                                );
                            }
                        }
                    }
                }
            }

            // Check generic param bounds (e.g., <T: Clone>)
            for gp in &generics {
                if !gp.bounds.is_empty() {
                    if let Some(concrete_type) = type_map.get(&gp.name) {
                        for bound in &gp.bounds {
                            if !self.type_implements_trait(concrete_type, bound) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0253,
                                    format!(
                                        "type '{}' does not implement trait '{}' (required by generic parameter '{}' of function '{}')",
                                        fmt_type(concrete_type),
                                        bound,
                                        gp.name,
                                        name
                                    ),
                                );
                            }
                        }
                    }
                }
            }

            // Check arguments with substituted types
            for (i, (arg, param)) in args.iter().zip(params.iter()).enumerate() {
                let at = self.infer_expr(arg, scopes);
                let subst_param = if !type_map.is_empty() {
                    subst_type_params(param, &generics, &type_map)
                } else {
                    param.clone()
                };
                if !same_type(&at, &subst_param) {
                    self.emit_code(
                        crate::diagnostic::codes::E0211,
                        format!(
                            "argument {} of '{}' expected {}, found {}",
                            i + 1,
                            name,
                            fmt_type(&subst_param),
                            fmt_type(&at)
                        ),
                    );
                }
            }
        }
        // Substitute type args into return type
        if !type_map.is_empty() {
            subst_type_params(&ret, &generics, &type_map)
        } else {
            ret
        }
    }
}
