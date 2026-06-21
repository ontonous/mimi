use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, same_type, suggest_name, subst_type_params};
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
        let obj_ty = self.infer_expr(obj, scopes);
        if let Type::Name(type_name, type_args) = &obj_ty {
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
            // Check module-qualified function call: Module::func(args)
            let qualified_func = format!("{}::{}", type_name, method_name);
            if self.funcs.contains_key(&qualified_func) {
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
                        let (method_params, method_ret) =
                            if let Some(trait_generic_names) = self.trait_generics.get(&trait_name) {
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
            self.check_option_method(method_name, inner, args, scopes)
        } else if let Type::Result(ok_ty, err_ty) = &obj_ty {
            self.check_result_method(method_name, ok_ty, err_ty, args, scopes)
        } else {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0222,
                    format!("method call requires a named type, found {}", fmt_type(&obj_ty)),
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
                    for (i, (arg, param)) in
                        user_args.iter().zip(method_params.iter()).enumerate()
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

    pub(in crate::core) fn infer_turbofish(
        &mut self,
        name: &str,
        type_args: &[Type],
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
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
