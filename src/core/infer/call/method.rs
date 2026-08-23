use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, is_numeric_coercion, subst_type_params, suggest_name};
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;

/// 0.36.6 (裁决 1 跨 flow 补全): strip the surface `flow::<name>::` prefix from
/// a nominal type. `::` cannot appear in user identifiers, so any
/// `flow::`-prefixed name is generator-made — the checker types Fault sink
/// call results as `flow::<flow>::Fault` so field access resolves the correct
/// per-flow record, and this normalizer lets those results unify against the
/// unqualified parameter/overload names the flow matrix registers (`Fault`).
fn strip_flow_qualifier(ty: &Type) -> Type {
    match ty.unlocated() {
        Type::Name(n, args) if n.starts_with("flow::") => {
            let short = n
                .rsplit("::")
                .next()
                .map(str::to_string)
                .unwrap_or_else(|| n.clone());
            Type::Name(short, args.clone())
        }
        _ => ty.clone(),
    }
}

impl<'a> Checker<'a> {
    /// 0.36.47: 把方法签名中残留的方法级泛型名字型（`Type::Name("U")`）替换为
    /// fresh TypeVar——同一名字在本方法的所有参数与返回类型间共享同一变量。
    pub(in crate::core) fn instantiate_method_generics(
        &mut self,
        params: &mut [Type],
        ret: &mut Type,
        names: &[String],
    ) {
        if names.is_empty() {
            return;
        }
        let mut type_map: HashMap<String, Type> = HashMap::new();
        let mut gen_slice: Vec<GenericParam> = Vec::with_capacity(names.len());
        for name in names {
            let fresh = self.fresh_var();
            type_map.insert(name.clone(), fresh);
            gen_slice.push(GenericParam {
                meta: AstNodeMeta::synthetic(AstOrigin::RuntimeSystem(
                    "infer.instantiate_method_generics",
                )),
                name: name.clone(),
                bounds: vec![],
                kind: crate::ast::GenericKind::Free,
            });
        }
        for param in params.iter_mut() {
            *param = subst_type_params(param, &gen_slice, &type_map);
        }
        *ret = subst_type_params(ret, &gen_slice, &type_map);
    }

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
        if let Expr::Ident(module_name) = obj.unlocated() {
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
                let arg_types: Vec<Type> = args
                    .iter()
                    .map(|arg| self.infer_expr(arg, scopes))
                    .collect();
                // FLOW-IDENTITY-001 linear generation: the source state (first
                // argument) is consumed by the transition. Mark the variable so
                // subsequent uses are rejected with E0423.
                if let Some(Expr::Ident(source_var)) = args.first().map(|a| a.unlocated()) {
                    self.consumed_flow_vars.insert(
                        source_var.clone(),
                        format!("{}::{}", module_name, method_name),
                    );
                    // 追加 B: mark linear consumption for ? ordering constraint
                    self.linear_consumed_before_try = true;
                }
                let from_ty = arg_types
                    .first()
                    .cloned()
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                // 0.36.6: a qualified flow result (e.g. `flow::Svc::Fault`) must
                // still match the unqualified overload key (`Fault`).
                let from_short = strip_flow_qualifier(&from_ty);
                // 0.36.10 (裁决 6 follow-up): recover/reset on a transition
                // result that DECLARED faultability (`-> S | Fault`) — the
                // static type is the first target, but at runtime the value
                // may be a Fault. Accept the value directly (statically
                // typing it as the first target); the runtime dispatches on
                // the actual tag. Recovery of a non-Faulted tag is a runtime
                // error in both backends. Cross-flow values stay rejected
                // (the arg must belong to THIS flow).
                let widened_recover_reset = matches!(method_name, "recover" | "reset")
                    && matches!(args.first().map(|a| a.unlocated()), Some(Expr::Ident(v))
                        if matches!(self.faultable_result_vars.get(v), Some(f) if f == module_name));
                let overload_key = if widened_recover_reset {
                    format!("{}::Fault", short_key)
                } else {
                    match from_short.unlocated() {
                        Type::Name(n, _) => format!("{}::{}", short_key, n),
                        _ => short_key.clone(),
                    }
                };
                let signature = self
                    .funcs
                    .get(&overload_key)
                    .or_else(|| self.funcs.get(&short_key))
                    .cloned();
                if let Some((params, ret_type)) = signature {
                    if arg_types.len() != params.len() {
                        self.emit_code(
                            crate::diagnostic::codes::E0257,
                            format!(
                                "flow transition '{}::{}' expects {} arguments, got {}",
                                module_name,
                                method_name,
                                params.len(),
                                arg_types.len()
                            ),
                        );
                    } else {
                        for (index, (actual, expected)) in
                            arg_types.iter().zip(params.iter()).enumerate()
                        {
                            // 0.36.10 (裁决 6 follow-up): the widened
                            // recover/reset path passes a statically
                            // first-target-typed value where the overload
                            // declares `Fault`; the runtime dispatch decides.
                            // Skip the from-state unify for that single arg.
                            if widened_recover_reset && index == 0 {
                                continue;
                            }
                            let actual_clean = strip_flow_qualifier(actual);
                            let coerced = is_numeric_coercion(expected, &actual_clean);
                            if !coerced && self.unification.unify(expected, &actual_clean).is_err()
                            {
                                // 0.1.8 Phase F (sparse DX): when the source
                                // state does not match this event, list the
                                // events that are legal from the actual state.
                                let mut legal_tail = String::new();
                                if index == 0 {
                                    let actual_state_name = match actual_clean.unlocated() {
                                        Type::Name(n, _) => n.clone(),
                                        _ => String::new(),
                                    };
                                    if !actual_state_name.is_empty() {
                                        let flow_prefix = format!("flow::{}::", module_name);
                                        let state_suffix = format!("::{}", actual_state_name);
                                        let mut legal_events: Vec<String> = self
                                            .funcs
                                            .keys()
                                            .filter_map(|key| {
                                                let rest = key.strip_prefix(&flow_prefix)?;
                                                let event = rest.strip_suffix(&state_suffix)?;
                                                if !event.is_empty() && !event.contains("::") {
                                                    Some(event.to_string())
                                                } else {
                                                    None
                                                }
                                            })
                                            .collect();
                                        legal_events.sort();
                                        if !legal_events.is_empty() {
                                            legal_tail = format!(
                                                "; legal events from {}: {}",
                                                actual_state_name,
                                                legal_events.join(", ")
                                            );
                                        }
                                    }
                                }
                                self.errors.push(Diagnostic::error_code(
                                    crate::diagnostic::codes::E0211,
                                    format!(
                                        "argument {} of flow transition '{}::{}' expected {}, found {}{}",
                                        index + 1,
                                        module_name,
                                        method_name,
                                        fmt_type(expected),
                                        fmt_type(actual),
                                        legal_tail
                                    ),
                                    self.diagnostic_span(),
                                ));
                            }
                        }
                    }
                    return ret_type;
                }
                let from_state_name = match from_short.unlocated() {
                    Type::Name(n, _) => n.clone(),
                    _ => String::new(),
                };
                // 0.1.8 Phase F (sparse DX): an unavailable (state, event) is
                // rejected at compile time; make the rejection actionable by
                // listing the currently-legal events from that state.
                let flow_prefix = format!("flow::{}::", module_name);
                let state_suffix = if from_state_name.is_empty() {
                    String::new()
                } else {
                    format!("::{}", from_state_name)
                };
                let mut legal_events: Vec<String> = self
                    .funcs
                    .keys()
                    .filter_map(|key| {
                        let rest = key.strip_prefix(&flow_prefix)?;
                        let event = rest.strip_suffix(&state_suffix)?;
                        if !state_suffix.is_empty() && !event.is_empty() && !event.contains("::") {
                            Some(event.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                legal_events.sort();
                let legal_tail = if legal_events.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; legal events from {}: {}",
                        from_state_name,
                        legal_events.join(", ")
                    )
                };
                self.emit_code(
                    crate::diagnostic::codes::E0211,
                    format!(
                        "no flow transition overload '{}::{}' accepts source state {}{}",
                        module_name,
                        method_name,
                        fmt_type(&from_ty),
                        legal_tail
                    ),
                );
                return Type::Name("unit".into(), vec![]);
            }
        }

        let obj_ty = self.infer_expr(obj, scopes);
        if std::env::var("MIMI_DBG_LK").is_ok() {
            eprintln!("DBG obj_ty={}", crate::core::fmt_type(&obj_ty));
        }
        // Capability method dispatch: Type::Cap(name) with split/drop.
        // Interp: Value::Cap(components); split() → Tuple of single-component
        // caps, drop() → unit. Checker mirrors the component count from the
        // cap declaration (0.34.19 CHECKER-GAP: aliases were unresolved).
        //
        // H5 (audit 2026-08-03): split components are ATOMIC — `Type::CapAtom`.
        // Previously the checker returned `Type::Cap(component)` and happily
        // re-expanded a split-out component (`ab.split()` where ab = A+B) while
        // the bytecode VM rejected it at runtime (E0800: single-component cap)
        // and codegen rejected it at compile time with a misleading E0700
        // ("method 'split' not compiled for type 'i64'" — the component had
        // degraded to an opaque handle). Three paths, three behaviors. Now the
        // checker rejects nested split on CapAtom, so run and build agree at
        // check time; the VM's E0800 stays as a defensive runtime guard.
        let cap_atom = matches!(obj_ty.unlocated(), Type::CapAtom(_));
        if let Type::Cap(cap_name) | Type::CapAtom(cap_name) = obj_ty.unlocated() {
            match method_name {
                "split" => {
                    if cap_atom {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0221,
                                format!(
                                    "capability '{}' is a split component and cannot be split again",
                                    cap_name
                                ),
                                self.diagnostic_span(),
                            )
                            .with_help(
                                "split() applies to a combined capability (e.g., cap FullAccess = Read + Write)",
                            ),
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let components = self
                        .cap_components
                        .get(cap_name)
                        .cloned()
                        .unwrap_or_else(|| vec![cap_name.clone()]);
                    // audit-flow H3 (2026-08-03): a single-component cap's
                    // split() diverged — interp rejected at runtime (E0800
                    // "requires a combined capability") while codegen compiled
                    // and ran successfully (L1 divergence). Reject here at
                    // check time so all three paths agree; the VM's E0800 stays
                    // as a defensive runtime guard.
                    if components.len() <= 1 {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0221,
                                format!(
                                    "capability '{}' is a single capability and cannot be split",
                                    cap_name
                                ),
                                self.diagnostic_span(),
                            )
                            .with_help(
                                "split() applies to a combined capability (e.g., cap FullAccess = Read + Write)",
                            ),
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let parts: Vec<Type> = components
                        .iter()
                        .map(|component| Type::CapAtom(component.clone()))
                        .collect();
                    return Type::Tuple(parts);
                }
                "drop" => return Type::Name("unit".into(), vec![]),
                _ => {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0221,
                            format!(
                                "capability '{}' has no method '{}' (available: split, drop)",
                                cap_name, method_name
                            ),
                            self.diagnostic_span(),
                        )
                        .with_help("capabilities support split() on combined caps and drop()"),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
            }
        }
        // Newtype delegates method dispatch using the newtype name.
        // e.g. UserId(42).id() looks up trait methods for "UserId".
        let (type_name, type_args): (&String, &[Type]) = match obj_ty.unlocated() {
            Type::Newtype(name, _) => (name, &[]),
            Type::Name(tn, ta) => (tn, ta.as_slice()),
            _ => {
                // fall through to the rest of the method (string/list/trait check below)
                (&String::new(), &[])
            }
        };
        if !type_name.is_empty() {
            // 0.1.8 Phase E: SessionChan method surface.
            // `ch.send(v)` == `session_send(ch, v)`, `ch.recv() == session_recv(ch)`,
            // `ch.close() == session_close(ch)`. Builtin methods deliberately
            // shadow trait impls (same ruling as List/Set/String methods).
            if type_name == "SessionChan" && type_args.len() == 1 {
                match method_name {
                    "send" | "recv" | "close" => {
                        return self.check_session_method(obj, &obj_ty, method_name, args, scopes)
                    }
                    _ => {}
                }
            }
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
            // Check if it's an actor spawn call (Type.spawn).
            // H-4 (audit 2026-08-05): the spawn decision previously sat BEFORE
            // any actor validation, so `"hello".spawn()` / `(1).spawn()` were
            // silently accepted (the receiver type name was returned as the
            // handle type). spawn() creates an actor instance and is only valid
            // on actor types — reject every other receiver with E0221.
            if method_name == "spawn" || method_name == "spawn_detached" {
                let is_actor = self
                    .file
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::Actor(actor) if &actor.name == type_name));
                if !is_actor {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0221,
                            format!(
                                "type '{}' has no method '{}' — spawn() is only valid on actor types",
                                type_name, method_name
                            ),
                            self.diagnostic_span(),
                        )
                        .with_help("declare an `actor` type and spawn it, e.g. `MyActor.spawn()`"),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
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
                    .map(|a| {
                        a.methods.iter().any(|m| m.name == *method_name)
                            // 0.35.14 (DX backlog #13, layer ①): an actor that
                            // `runs` a Flow dispatches transitions as methods.
                            || self.runs_flow_transition(type_name, method_name).is_some()
                    })
                    .unwrap_or(false);
                if is_actor_method {
                    // Avoid .expect: re-resolve actor/method with if-let.
                    if let Some(actor) = self.file.items.iter().find_map(|item| match item {
                        Item::Actor(a) if a.name == *type_name => Some(a),
                        _ => None,
                    }) {
                        if let Some(method) = actor.methods.iter().find(|m| m.name == *method_name)
                        {
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
                                for (i, (arg, param)) in
                                    args.iter().zip(method.params.iter()).enumerate()
                                {
                                    self.reject_narrow_across_mailbox_arg(arg, scopes);
                                    let declared = self.resolve_type(&param.ty);
                                    let at = self.infer_expr(arg, scopes);
                                    // IF-C1/C4: strict unify rejects escape hatches at call sites.
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
                    }
                    // 0.35.14 (DX backlog #13, layer ①): runs_flow transition
                    // synthetic method — signature mirrors checker/items.rs
                    // registration: (self, event params…) -> ToState; with
                    // `fails E` the return is Result<ToState, (FromState, E)>.
                    if let Some(transition) = self.runs_flow_transition(type_name, method_name) {
                        let method_params: Vec<Type> = transition
                            .params
                            .iter()
                            .map(|p| self.resolve_type(&p.ty))
                            .collect();
                        let target = transition
                            .to_states
                            .first()
                            .map(|s| Type::Name(s.clone(), vec![]))
                            .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                        let ret = if let Some(err_ty) = &transition.fails {
                            let err_tuple = Type::Tuple(vec![
                                Type::Name(transition.from_state.clone(), vec![]),
                                self.resolve_type(err_ty),
                            ]);
                            Type::Result(Box::new(target), Box::new(err_tuple))
                        } else {
                            target
                        };
                        if args.len() != method_params.len() {
                            self.emit_code(
                                crate::diagnostic::codes::E0257,
                                format!(
                                    "method '{}' of actor '{}' expects {} arguments, got {}",
                                    method_name,
                                    type_name,
                                    method_params.len(),
                                    args.len()
                                ),
                            );
                        } else {
                            for (i, (arg, param)) in
                                args.iter().zip(method_params.iter()).enumerate()
                            {
                                let at = self.infer_expr(arg, scopes);
                                // 0.36.6: qualified flow results unify against
                                // the unqualified runs_flow param names.
                                if self
                                    .unification
                                    .unify(&strip_flow_qualifier(&at), param)
                                    .is_err()
                                {
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
                    return Type::Name("unknown".into(), vec![]);
                }
                return self.check_call(&qualified_func, args, scopes);
            }
            // Check record field access (field is a closure/function)
            // 0.34.35b (M-011③): record 的 fn 字段带参数直接调用不支持——
            // 此前参数被静默吞掉（返回字段类型，args 不检查），最终以
            // lowering 的 TOOL-RESOLUTION-001 内部标志拒绝，诊断误导。
            // 冻结纪律：不加新语法，只让报错说真话。`let f = vt.add; f(1,2)`
            // 是支持的取值路径（N-2 已修）；`vt.add(1,2)` 明确指导先绑定。
            if let Some(tdef) = self.types.get(type_name) {
                if let TypeDefKind::Record(fields) = &tdef.kind {
                    if let Some(f) = fields.iter().find(|f| f.name == method_name) {
                        let field_ty = self.resolve_type(&f.ty);
                        if matches!(field_ty.unlocated(), Type::Func(..) | Type::ExternFunc(..)) {
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0223,
                                    format!(
                                        "callee must be a function name: field '{}' of '{}' is a function value and cannot be invoked directly on the record",
                                        method_name, type_name
                                    ),
                                    self.diagnostic_span(),
                                )
                                .with_help(format!(
                                    "bind the field first, then call it: let f = obj.{0}; f(...)",
                                    method_name
                                )),
                            );
                            return Type::Name("unknown".into(), vec![]);
                        }
                        return field_ty;
                    }
                }
                if let TypeDefKind::Enum(variants) = &tdef.kind {
                    if variants.iter().any(|v| v.name == method_name) {
                        return Type::Name(type_name.clone(), vec![]);
                    }
                }
            }
            // Generic type parameter with declared trait bound:
            // `x.clone()` where x: T and T: Clone dispatches to the bound
            // trait's method signature, instantiated with the type param
            // (0.34.19 CHECKER-GAP: bound methods were unresolved → E0221).
            if !self.types.contains_key(type_name) {
                let func_name = self
                    .current_callable_owner
                    .as_ref()
                    .map(|owner| owner.0.strip_prefix("function:").unwrap_or(&owner.0))
                    .map(|name| name.rsplit("::").next().unwrap_or(name))
                    .unwrap_or_default();
                let bounds: Vec<String> = self
                    .func_generics
                    .get(func_name)
                    .map(|generics| {
                        generics
                            .iter()
                            .filter(|gp| gp.name == *type_name)
                            .flat_map(|gp| gp.bounds.iter().cloned())
                            .collect()
                    })
                    .filter(|bs: &Vec<String>| !bs.is_empty())
                    .or_else(|| {
                        self.where_clauses.get(func_name).map(|entries| {
                            entries
                                .iter()
                                .filter(|(tp, _)| tp == type_name)
                                .flat_map(|(_, bs)| bs.iter().cloned())
                                .collect()
                        })
                    })
                    .unwrap_or_default();
                if !bounds.is_empty() {
                    // Built-in Clone bound: `clone` copies the value (Mimi
                    // assignment semantics) — signature clone(x: T) -> T.
                    if bounds.iter().any(|b| b == "Clone") && method_name == "clone" {
                        return Type::Name(type_name.clone(), type_args.to_vec());
                    }
                    // User trait bound: look up the declared method signature
                    // and instantiate the trait generics with the type args.
                    for bound in &bounds {
                        if let Some((_, ret)) = self
                            .trait_method_sigs
                            .get(&(bound.clone(), method_name.to_string()))
                            .cloned()
                        {
                            // §2-#19 (audit 2026-08-05, closed 2026-08-07):
                            // bound-generic 用户 trait 方法调用没有后端支撑——
                            // lowering 无法为泛型接收者选 impl（需单态化，
                            // 1.x 评估），此前以内部 TOOL-RESOLUTION-001 拒绝
                            // 连正确调用也拒，诊断误导。冻结纪律：checker
                            // 前置诚实拒绝（E0437），Clone 例外（lower 拷贝
                            // 语义特化，端到端可用）。返回推断类型避免下游
                            // 级联错报。
                            self.errors.push(
                                Diagnostic::error_code(
                                    crate::diagnostic::codes::E0437,
                                    format!(
                                        "trait method '{method_name}' cannot be dispatched on generic parameter '{type_name}' (bound '{bound}'): monomorphization is deferred to 1.x"
                                    ),
                                    self.diagnostic_span(),
                                )
                                .with_help(
                                    "call the trait method on a concrete type, or take the concrete type as parameter instead of a bounded generic",
                                ),
                            );
                            let ret = if let Some(tg) = self.trait_generics.get(bound) {
                                if !tg.is_empty() && tg.len() == type_args.len() {
                                    let type_map: HashMap<String, Type> = tg
                                        .iter()
                                        .zip(type_args.iter())
                                        .map(|(g, a)| (g.clone(), a.clone()))
                                        .collect();
                                    let gen_slice: Vec<GenericParam> = tg
                                        .iter()
                                        .map(|name| GenericParam {
                                            meta: AstNodeMeta::synthetic(AstOrigin::RuntimeSystem(
                                                "infer.bound_generic_substitution",
                                            )),
                                            name: name.clone(),
                                            bounds: vec![],
                                            kind: crate::ast::GenericKind::Free,
                                        })
                                        .collect();
                                    subst_type_params(&ret, &gen_slice, &type_map)
                                } else {
                                    ret
                                }
                            } else {
                                ret
                            };
                            return ret;
                        }
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
                                        meta: AstNodeMeta::synthetic(AstOrigin::RuntimeSystem(
                                            "infer.trait_generic_substitution",
                                        )),
                                        name: g.clone(),
                                        bounds: vec![],
                                        kind: crate::ast::GenericKind::Free,
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
                        // 0.36.47: 方法级泛型（`func map<U>` 的 U）实例化为 fresh
                        // TypeVar——signature 里的 U 在注册期是名字型，不实例化
                        // 则 unify 永远失败（E0211「expected fn(T) -> U, found
                        // fn(T) -> T」）；实例化后 U 经实参推断绑定，返回类型
                        // zonk 出具体型。
                        let mut method_params = method_params;
                        let mut method_ret = method_ret;
                        if let Some(mg) = self
                            .trait_method_generics
                            .get(&(trait_name.clone(), method_name.to_string()))
                            .cloned()
                        {
                            self.instantiate_method_generics(
                                &mut method_params,
                                &mut method_ret,
                                &mg,
                            );
                        }
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
                                // 0.39.62 (Phase C): trait 方法 dispatch 必须与
                                // simple.rs / impl 方法同款执行线性实参种类检查。
                                // 此前此路径完全绕过——Free-T 泄漏方法体 + 线性实参
                                // 静默弃值（pre-existing soundness 洞）。
                                if self.is_linear_surface_type(&at) {
                                    self.check_method_linear_arg_kind(
                                        type_name,
                                        method_name,
                                        i,
                                        method_params.len(),
                                        &at,
                                    );
                                }
                                // IF-C1/C5: strict unify rejects escape hatches at call sites.
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
                        return self.unification.zonk_or_unknown(&method_ret);
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
                    // H3: check user-facing args (skip leading `self` if present).
                    let method_params: Vec<Type> = method
                        .params
                        .iter()
                        .filter(|p| p.name != "self")
                        .map(|p| self.resolve_type(&p.ty))
                        .collect();
                    if args.len() != method_params.len() {
                        self.emit_code(
                            crate::diagnostic::codes::E0257,
                            format!(
                                "method '{}' of actor '{}' expects {} arguments, got {}",
                                method_name,
                                type_name,
                                method_params.len(),
                                args.len()
                            ),
                        );
                    } else {
                        for (i, (arg, param)) in args.iter().zip(method_params.iter()).enumerate() {
                            self.reject_narrow_across_mailbox_arg(arg, scopes);
                            let at = self.infer_expr(arg, scopes);
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
                // 0.35.14 (DX backlog #13): transitions are callable on
                // runs_flow actors — include them in typo suggestions.
                if let Some(flow_name) = actor_def.runs_flow.as_deref() {
                    if let Some(flow) = self.file.items.iter().find_map(|item| match item {
                        Item::Flow(f) if f.name == flow_name => Some(f),
                        _ => None,
                    }) {
                        method_candidates.extend(flow.transitions.iter().map(|t| t.name.clone()));
                    }
                }
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
                    self.diagnostic_span(),
                )
                .with_help(&help),
            );
            Type::Name("unknown".into(), vec![])
        } else if let Type::DynTrait(traits) = obj_ty.unlocated() {
            self.resolve_trait_method(traits, method_name, args, scopes)
        } else if let Type::ImplTrait(traits) = obj_ty.unlocated() {
            self.resolve_trait_method(traits, method_name, args, scopes)
        } else if let Type::Option(inner) = obj_ty.unlocated() {
            // Codegen supports `.deref()` on `Option<shared T>`
            // (produced by `weak.upgrade()`), where deref extracts the shared payload.
            if method_name == "deref" && matches!(inner.unlocated(), Type::Shared(_)) {
                match inner.unlocated() {
                    Type::Shared(i) => (**i).clone(),
                    _ => Type::Name("unknown".into(), vec![]),
                }
            } else {
                self.check_option_method(method_name, inner, args, scopes)
            }
        } else if let Type::Result(ok_ty, err_ty) = obj_ty.unlocated() {
            self.check_result_method(method_name, ok_ty, err_ty, args, scopes)
        } else if let Type::Shared(inner) = obj_ty.unlocated() {
            self.check_shared_method(method_name, inner)
        } else if let Type::Weak(inner) = obj_ty.unlocated() {
            self.check_weak_method(method_name, inner)
        } else {
            self.errors.push(
                Diagnostic::error_code(
                    crate::diagnostic::codes::E0222,
                    format!(
                        "method call requires a named type, found {}",
                        fmt_type(&obj_ty)
                    ),
                    self.diagnostic_span(),
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
                // 0.36.47: trait 对象面同款——方法级泛型名实例化（DynTrait 无法
                // 从接收者解 T，但 U 至少须可绑）。
                let mut method_params = params;
                let mut method_ret = ret;
                if let Some(mg) = self
                    .trait_method_generics
                    .get(&(trait_name.clone(), method_name.to_string()))
                    .cloned()
                {
                    self.instantiate_method_generics(&mut method_params, &mut method_ret, &mg);
                }
                let method_params = &method_params;
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
                        // IF-C5 residual: unify so TypeVars resolve.
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
                return self.unification.zonk_or_unknown(&method_ret);
            }
        }
        self.errors.push(
            Diagnostic::error_code(
                crate::diagnostic::codes::E0221,
                format!("trait object does not have method '{}'", method_name),
                self.diagnostic_span(),
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
                        self.diagnostic_span(),
                    )
                    .with_help("shared values support clone, deref, inner"),
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
                        self.diagnostic_span(),
                    )
                    .with_help("weak values support upgrade"),
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
            // 追加 E: reject unconstrained type variables in from_json::<T>
            // The deserialization target type must be fully resolved at compile time.
            let target_ty = &type_args[0];
            if crate::core::unification::scan_residual(target_ty).is_err() {
                self.emit_code(
                    crate::diagnostic::codes::E0430,
                    "from_json::<T> requires a concrete type argument, found unconstrained type. Use from_json::<ConcreteType>(s) to specify the deserialization target",
                );
            }
            // Audit §2-#16 (VERIFIED 2026-08-05): from_json::<T> only checked
            // for unconstrained residuals — a *linear* target type slipped
            // through, letting JSON fabricate a capability value out of thin
            // air (bypassing exactly-once / linear consumption entirely).
            // H2 ruling (AGENTS.md §0): containers carrying linear elements
            // are equally forbidden, so use the deep is_linear_surface_type.
            if self.is_linear_surface_type(target_ty) {
                self.emit_code(
                    crate::diagnostic::codes::E0432,
                    format!(
                        "from_json::<{}> cannot deserialize a linear type — the JSON string would fabricate a capability value. Use a concrete non-linear type argument",
                        fmt_type(target_ty)
                    ),
                );
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
        // 0.36.32: session_open::<S>() — typed session endpoint construction.
        // The residual engine (E0414/E0425/E0426) is live on SessionChan<S>
        // values; the plain session_open() form types as bare SessionChan
        // (no S), and the generic turbofish path only consults user funcs
        // (E0401 — the 0.36.23 dead-face). Return SessionChan<S> so the
        // endpoint actually carries the protocol residual.
        if name == "session_open" {
            if type_args.len() != 1 {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    "session_open::<S> expects exactly 1 type argument (a declared \
                     session name)",
                );
                return Type::Name("unknown".into(), vec![]);
            }
            let s = &type_args[0];
            let s_name = match s.unlocated() {
                Type::Name(n, args) if args.is_empty() => n.clone(),
                _ => {
                    self.emit_code(
                        crate::diagnostic::codes::E0413,
                        format!(
                            "session_open type argument must be a declared session \
                             name, found {}",
                            fmt_type(s)
                        ),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
            };
            if !self.session_types.contains_key(&s_name) {
                self.emit_code(
                    crate::diagnostic::codes::E0413,
                    format!(
                        "session_open::<{}> — '{}' is not a declared session type",
                        s_name, s_name
                    ),
                );
                return Type::Name("unknown".into(), vec![]);
            }
            if !args.is_empty() {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    "session_open takes no arguments",
                );
            }
            return Type::Name("SessionChan".into(), vec![s.clone()]);
        }
        // 0.36.38 (Phase C, §4d option (A)): session_pair::<S>() — the typed
        // PAIR form. Returns (SessionChan<S>, SessionChan<dual S>): the lo end
        // speaks S, the hi end speaks the dual (send on lo ↔ recv on hi,
        // matching the cross-wired runtime). Both endpoints carry residuals
        // (residual_from_chan_type seeds the dual expression), so the
        // compile-time protocol proof spans BOTH ends of the channel pair —
        // the 0.36.23 "dead face" (raw i64 handles) is closed on the pair
        // form too. Migration: `let pair = session_pair(); pair[i]` →
        // `let (lo, hi) = session_pair::<S>()`.
        if name == "session_pair" {
            if type_args.len() != 1 {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    "session_pair::<S> expects exactly 1 type argument (a declared \
                     session name)",
                );
                return Type::Name("unknown".into(), vec![]);
            }
            let s = &type_args[0];
            let s_name = match s.unlocated() {
                Type::Name(n, args) if args.is_empty() => n.clone(),
                _ => {
                    self.emit_code(
                        crate::diagnostic::codes::E0413,
                        format!(
                            "session_pair type argument must be a declared session \
                             name, found {}",
                            fmt_type(s)
                        ),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
            };
            if !self.session_types.contains_key(&s_name) {
                self.emit_code(
                    crate::diagnostic::codes::E0413,
                    format!(
                        "session_pair::<{}> — '{}' is not a declared session type",
                        s_name, s_name
                    ),
                );
                return Type::Name("unknown".into(), vec![]);
            }
            if !args.is_empty() {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    "session_pair takes no arguments",
                );
            }
            let dual_arg = Type::Name("dual".into(), vec![s.clone()]);
            return Type::Tuple(vec![
                Type::Name("SessionChan".into(), vec![s.clone()]),
                Type::Name("SessionChan".into(), vec![dual_arg]),
            ]);
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
            // Check where constraints (before substitution). CK-H6: all entries.
            if let Some(clauses) = self.where_clauses.get(name).cloned() {
                for (type_param, bounds) in clauses {
                    if let Some(concrete_type) = type_map.get(&type_param) {
                        for bound in &bounds {
                            if !self.type_implements_trait(concrete_type, bound) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0253,
                                    format!(
                                        "where constraint violated: type '{}' does not implement trait '{}' (required by function '{}')",
                                        fmt_type(concrete_type),
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
                // C2 (audit-type 2026-08-03): the turbofish instantiation path
                // must enforce the same linear-argument rejection as the
                // inferred-instantiation path in check_call (simple.rs) —
                // otherwise `func::<cap X>(cap_value)` silently escapes
                // exactly-once (E0432). Non-generic callees keep concrete
                // linear tracking (no rejection).
                // 0.36.39: 线性黑盒直通豁免（同 simple.rs 全局调用臂）——调体
                // 对 T 线性性零依赖则放行，否则 E0432；SessionChan 走 transfer-only。
                let bb_reject = if !generics.is_empty() && self.is_linear_surface_type(&at) {
                    // 0.1.9 Phase A: `linear T` 参数 kind 兼容，定义时已体校验，放行。
                    if self.param_uses_linear_kind(name, i) {
                        // 0.39.58: `linear drop T` 实例化必须可 drop——SessionChan 拒。
                        if self.param_uses_linear_drop_kind(name, i)
                            && self.surface_type_contains_session(&at)
                        {
                            self.emit_code(
                                crate::diagnostic::codes::E0432,
                                format!(
                                    "linear type '{}' cannot instantiate `linear drop T` (argument {} of function '{}'): \
                                     `linear drop T` requires a drop-tolerant type, but SessionChan cannot be \
                                     dropped (only transferred/closed). Use `linear T` for transfer-only",
                                    fmt_type(&at),
                                    i + 1,
                                    name
                                ),
                            );
                            true
                        } else {
                            false
                        }
                    } else {
                        // 0.39.59 (Phase C): Free `T` + 线性实参 → 一律 E0432
                        // （种类不匹配），退役调用点体分析。
                        true
                    }
                } else {
                    false
                };
                if bb_reject {
                    self.emit_code(
                        crate::diagnostic::codes::E0432,
                        format!(
                            "linear type '{}' cannot be passed as generic argument {} of function '{}': \
                             Free generic parameter `T` may only instantiate to non-linear types \
                             (kind mismatch). Declare the parameter kind `linear T` (transfer-only body) \
                             or `linear drop T` (drop-tolerant body), or use a concrete function \
                             signature taking the linear type directly",
                            fmt_type(&at),
                            i + 1,
                            name
                        ),
                    );
                }
                let subst_param = if !type_map.is_empty() {
                    subst_type_params(param, &generics, &type_map)
                } else {
                    param.clone()
                };
                // IF residual: strict unify at call sites.
                match self.unification.unify(&at, &subst_param) {
                    Err(crate::core::unification::UnifyError::LinearContainerEscape(msg)) => {
                        // C-1 (audit 2026-08-05): bare-container method
                        // parameters must not accept linear elements.
                        self.emit_code(
                            crate::diagnostic::codes::E0432,
                            format!(
                                "argument {} of '{}' carries a linear value into a bare container: {}",
                                i + 1,
                                name,
                                msg
                            ),
                        );
                    }
                    Err(_) => {
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
                    Ok(()) => {}
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

    /// 0.35.14 (DX backlog #13, layer ①): find the flow transition an actor
    /// dispatches through when it declares `runs FlowName`. Explicit actor
    /// methods take precedence (mirrors the synthetic-method registration in
    /// checker/items.rs), so a colliding name returns `None` here.
    /// 0.39.62 (Phase C): trait 方法调用点线性实参种类检查——与 simple.rs /
    /// impl 方法同款。`type_name` 实现类型、`method_name` 方法名、`arg_index` 为
    /// 调用侧实参下标（不含 self）、`decl_params_len` 为 trait 签名参数长度。
    /// funcs 注册含隐式 self@0 → funcs_index = arg_index + (funcs.len - decl_len)。
    fn check_method_linear_arg_kind(
        &mut self,
        type_name: &str,
        method_name: &str,
        arg_index: usize,
        decl_params_len: usize,
        at: &crate::ast::Type,
    ) {
        // 简单 key（trait_args 为空）；泛型 impl 的多义 key 取不到 → 保守
        // fail-closed（param_uses_linear_kind false → E0432）。
        let key = format!("{}_{}", type_name, method_name);
        // 非泛型方法（func_generics 无条目）→ 具体线性位置由 concrete 追踪处理，
        // E0432 种类规则不适用（`func take(x: cap FileReadCap)` 直接收 cap）。
        if !self.func_generics.contains_key(&key) {
            return;
        }
        let funcs_offset = self
            .funcs
            .get(&key)
            .map(|(ps, _)| ps.len().saturating_sub(decl_params_len))
            .unwrap_or(1);
        let funcs_index = arg_index + funcs_offset;
        if self.param_uses_linear_kind(&key, funcs_index) {
            // 0.39.58: `linear drop T` 实例化必须可 drop——SessionChan 拒。
            if self.param_uses_linear_drop_kind(&key, funcs_index)
                && self.surface_type_contains_session(at)
            {
                self.emit_code(
                    crate::diagnostic::codes::E0432,
                    format!(
                        "linear type '{}' cannot instantiate `linear drop T` (argument {} of method '{}'): \
                         `linear drop T` requires a drop-tolerant type, but SessionChan cannot be \
                         dropped (only transferred/closed). Use `linear T` for transfer-only",
                        fmt_type(at),
                        arg_index + 1,
                        method_name
                    ),
                );
            }
        } else {
            // Free T + 线性实参 → E0432（种类不匹配 + 迁移提示）。
            self.emit_code(
                crate::diagnostic::codes::E0432,
                format!(
                    "linear type '{}' cannot be passed as generic argument {} of method '{}': \
                     Free generic parameter `T` may only instantiate to non-linear types \
                     (kind mismatch). Declare the parameter kind `linear T` (transfer-only body) \
                     or `linear drop T` (drop-tolerant body), or use a concrete function \
                     signature taking the linear type directly",
                    fmt_type(at),
                    arg_index + 1,
                    method_name
                ),
            );
        }
    }

    fn runs_flow_transition(&self, actor_name: &str, method_name: &str) -> Option<&TransitionDef> {
        let actor = self.file.items.iter().find_map(|item| match item {
            Item::Actor(a) if a.name == actor_name => Some(a),
            _ => None,
        })?;
        if actor.methods.iter().any(|m| m.name == method_name) {
            return None;
        }
        let flow_name = actor.runs_flow.as_deref()?;
        let flow = self.file.items.iter().find_map(|item| match item {
            Item::Flow(f) if f.name == flow_name => Some(f),
            _ => None,
        })?;
        flow.transitions.iter().find(|t| t.name == method_name)
    }
}
