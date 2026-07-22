use crate::ast::*;
use crate::core::helpers::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

use super::Checker;

impl<'a> Checker<'a> {
    fn item_span(item: &Item) -> Span {
        match item {
            Item::Func(value) => value.meta.span,
            Item::Module(value) => value.meta.span,
            Item::Type(value) => value.meta.span,
            Item::Actor(value) => value.meta.span,
            Item::Cap(value) => value.meta.span,
            Item::Trait(value) => value.meta.span,
            Item::Impl(value) => value.meta.span,
            Item::ExternBlock(value) => value.meta.span,
            Item::Const { meta, .. } => meta.span,
            Item::Flow(value) => value.meta.span,
            Item::Protocol(value) => value.meta.span,
            Item::Session(value) => value.meta.span,
        }
    }

    pub(crate) fn collect_decls(&mut self) {
        // Process imports: add module names to use_imports
        for import in &self.file.imports {
            let module_name = import
                .alias
                .as_deref()
                .or_else(|| import.path.first().map(|s| s.as_str()))
                .map(|s| s.to_string());
            if let Some(name) = module_name {
                self.use_imports.push(name);
            }
        }
        // Register built-in Record types used by builtins
        self.register_builtin_types();
        for item in &self.file.items {
            self.collect_item_decls(item);
        }
        // Check for type alias cycles
        self.check_alias_cycles();
    }

    fn register_builtin_types(&mut self) {
        let builtin_meta = AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("checker.builtin_type"));
        let builtin_type =
            |name: &str| Type::Name(name.to_string(), vec![]).deep_reorigin(builtin_meta);
        // ExecResult { exit_code: i32, stdout: string, stderr: string }
        if !self.types.contains_key("ExecResult") {
            let td = TypeDef {
                meta: builtin_meta,
                name: "ExecResult".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        meta: builtin_meta,
                        name: "exit_code".to_string(),
                        ty: builtin_type("i32"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "stdout".to_string(),
                        ty: builtin_type("string"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "stderr".to_string(),
                        ty: builtin_type("string"),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            self.types.insert("ExecResult".to_string(), td);
        }
        // StatResult { size: i64, modified: i64, is_file: bool, is_dir: bool }
        if !self.types.contains_key("StatResult") {
            let td = TypeDef {
                meta: builtin_meta,
                name: "StatResult".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        meta: builtin_meta,
                        name: "size".to_string(),
                        ty: builtin_type("i64"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "modified".to_string(),
                        ty: builtin_type("i64"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "is_file".to_string(),
                        ty: builtin_type("bool"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "is_dir".to_string(),
                        ty: builtin_type("bool"),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            self.types.insert("StatResult".to_string(), td);
        }
        // v0.29.20 PeerFault — link-disconnect event payload (peer actor faulted).
        // { peer_id: string, reason: string }
        if !self.types.contains_key("PeerFault") {
            let td = TypeDef {
                meta: builtin_meta,
                name: "PeerFault".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        meta: builtin_meta,
                        name: "peer_id".to_string(),
                        ty: builtin_type("string"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "reason".to_string(),
                        ty: builtin_type("string"),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            self.types.insert("PeerFault".to_string(), td);
        }
        // v0.29.12 SystemTrace — structured Fault crash context
        // v0.29.39: added memory_dump + panic_payload structured fields
        // { last_state_name: string, unexpected_event: string, snapshot: string,
        //   memory_dump: MemoryDump, panic_payload: PanicPayload }
        if !self.types.contains_key("SystemTrace") {
            let td = TypeDef {
                meta: builtin_meta,
                name: "SystemTrace".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        meta: builtin_meta,
                        name: "last_state_name".to_string(),
                        ty: builtin_type("string"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "unexpected_event".to_string(),
                        ty: builtin_type("string"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "snapshot".to_string(),
                        ty: builtin_type("string"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "memory_dump".to_string(),
                        ty: builtin_type("MemoryDump"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "panic_payload".to_string(),
                        ty: builtin_type("PanicPayload"),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            self.types.insert("SystemTrace".to_string(), td);
        }
        // v0.29.39: PanicPayload — structured panic info
        // { error_type: string, file: string, line: i32, stack: string }
        if !self.types.contains_key("PanicPayload") {
            let td = TypeDef {
                meta: builtin_meta,
                name: "PanicPayload".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        meta: builtin_meta,
                        name: "error_type".to_string(),
                        ty: builtin_type("string"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "file".to_string(),
                        ty: builtin_type("string"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "line".to_string(),
                        ty: builtin_type("i32"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "stack".to_string(),
                        ty: builtin_type("string"),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            self.types.insert("PanicPayload".to_string(), td);
        }
        // v0.29.39: MemoryDump — field→value snapshot (string summary)
        // { fields: string, count: i32 }
        if !self.types.contains_key("MemoryDump") {
            let td = TypeDef {
                meta: builtin_meta,
                name: "MemoryDump".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        meta: builtin_meta,
                        name: "fields".to_string(),
                        ty: builtin_type("string"),
                    },
                    Field {
                        meta: builtin_meta,
                        name: "count".to_string(),
                        ty: builtin_type("i32"),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            self.types.insert("MemoryDump".to_string(), td);
        }
    }

    /// Detect type alias cycles: type A = B; type B = A;
    pub(crate) fn check_alias_cycles(&mut self) {
        let alias_names: Vec<String> = self.aliases.keys().cloned().collect();
        for name in &alias_names {
            let mut visited = std::collections::HashSet::new();
            visited.insert(name.clone());
            if self.follows_alias_cycle(name, &visited) {
                let span = self.alias_spans.get(name).copied().unwrap_or(Span::UNKNOWN);
                self.errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0409,
                    format!("type alias cycle detected: '{}' forms a cycle", name),
                    span,
                ));
            }
        }
    }

    pub(crate) fn follows_alias_cycle(
        &self,
        name: &str,
        visited: &std::collections::HashSet<String>,
    ) -> bool {
        if let Some(target) = self.aliases.get(name) {
            // Extract all named type references from the alias target
            let names = Self::extract_type_names(target);
            for target_name in names {
                if visited.contains(&target_name) {
                    return true;
                }
                if self.aliases.contains_key(&target_name) {
                    let mut new_visited = visited.clone();
                    new_visited.insert(target_name.clone());
                    if self.follows_alias_cycle(&target_name, &new_visited) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Extract all top-level type names referenced in a type (recursing into containers).
    fn extract_type_names(ty: &Type) -> Vec<String> {
        match ty {
            Type::Located { ty, .. } => Self::extract_type_names(ty),
            Type::Name(name, args) => {
                let mut names = vec![name.clone()];
                for a in args {
                    names.extend(Self::extract_type_names(a));
                }
                names
            }
            Type::Ref(_, inner)
            | Type::RefMut(_, inner)
            | Type::Option(inner)
            | Type::Shared(inner)
            | Type::LocalShared(inner)
            | Type::Weak(inner)
            | Type::WeakLocal(inner)
            | Type::Array(inner, _)
            | Type::Slice(inner)
            | Type::RawPtr(inner)
            | Type::RawPtrMut(inner)
            | Type::CShared(inner)
            | Type::CBorrow(inner)
            | Type::CBorrowMut(inner)
            | Type::CBuffer(inner) => Self::extract_type_names(inner),
            Type::Result(ok, err) => {
                let mut names = Self::extract_type_names(ok);
                names.extend(Self::extract_type_names(err));
                names
            }
            Type::Tuple(elems) => {
                let mut names = Vec::new();
                for e in elems {
                    names.extend(Self::extract_type_names(e));
                }
                names
            }
            Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
                let mut names = Vec::new();
                for a in args {
                    names.extend(Self::extract_type_names(a));
                }
                names.extend(Self::extract_type_names(ret));
                names
            }
            Type::Newtype(_, inner) => Self::extract_type_names(inner),
            _ => Vec::new(),
        }
    }

    pub(crate) fn collect_item_decls(&mut self, item: &Item) {
        self.set_span(Self::item_span(item));
        match item {
            Item::Func(f) => {
                self.set_span(f.meta.span);
                let qualified_name = if self.module_path.is_empty() {
                    f.name.clone()
                } else {
                    format!("{}::{}", self.module_path.join("::"), f.name)
                };
                if self.funcs.contains_key(&qualified_name) {
                    self.emit_code(
                        crate::diagnostic::codes::E0402,
                        format!("duplicate function definition '{}'", qualified_name),
                    );
                    return;
                }
                let generic_names: Vec<String> =
                    f.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                let params: Vec<Type> = f.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                let mut ret = f
                    .ret
                    .as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                // Lifetime elision: if return type has elided lifetimes (Ref(None, _)) and
                // exactly one unique named lifetime exists in the parameter types, apply it.
                let has_elided_lifetime = type_contains_elided_lifetime(&ret);
                if has_elided_lifetime {
                    let mut param_lifetimes: Vec<String> = Vec::new();
                    for p in &params {
                        param_lifetimes.extend(collect_lifetimes(p));
                    }
                    param_lifetimes.sort();
                    param_lifetimes.dedup();
                    if param_lifetimes.len() == 1 {
                        ret = elide_lifetime(&ret, &param_lifetimes[0]);
                    }
                }
                let allow_passport = f.extern_abi.is_some();
                for (i, p) in f.params.iter().enumerate() {
                    if allow_passport {
                        self.check_type_well_formed_allow_passport(
                            &params[i],
                            &format!("parameter '{}' of function '{}'", p.name, qualified_name),
                        );
                    } else {
                        self.check_type_well_formed(
                            &params[i],
                            &format!("parameter '{}' of function '{}'", p.name, qualified_name),
                        );
                    }
                }
                if allow_passport {
                    self.check_type_well_formed_allow_passport(
                        &ret,
                        &format!("return type of function '{}'", qualified_name),
                    );
                } else {
                    self.check_type_well_formed(
                        &ret,
                        &format!("return type of function '{}'", qualified_name),
                    );
                }
                self.generic_scope
                    .truncate(self.generic_scope.len() - generic_names.len());
                // For async functions, the declared return type is wrapped in Future<T>.
                // e.g., `async func foo() -> i32` has signature `foo() -> Future<i32>`.
                let func_sig_ret = if f.is_async {
                    Type::Name("Future".into(), vec![ret])
                } else {
                    ret
                };
                self.funcs
                    .insert(qualified_name.clone(), (params, func_sig_ret));
                // Store generic parameters if present
                if !f.generics.is_empty() {
                    self.func_generics
                        .insert(qualified_name.clone(), f.generics.clone());
                }
                // Store where clause if present.
                // CK-H6: accumulate ALL type-param bounds (do not overwrite).
                if !f.where_clause.is_empty() {
                    let entry = self.where_clauses.entry(f.name.clone()).or_default();
                    for wc in &f.where_clause {
                        entry.push((wc.type_param.clone(), wc.bounds.clone()));
                    }
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
                    self.emit_code(
                        crate::diagnostic::codes::E0402,
                        format!("duplicate type definition '{}'", t.name),
                    );
                    return;
                }
                let generic_names: Vec<String> =
                    t.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                // For Record/Union/Enum (structural types), insert into self.types before
                // checking fields to allow recursive self-references (e.g. type Expr { Call(name: string, args: List<Expr>) }).
                // Alias and Newtype are checked by check_alias_cycles instead.
                let allow_recursive = matches!(
                    &t.kind,
                    TypeDefKind::Record(_) | TypeDefKind::Union(_) | TypeDefKind::Enum(_)
                );
                if allow_recursive {
                    self.types.insert(t.name.clone(), t.clone());
                    if !t.generics.is_empty() {
                        self.type_generics
                            .insert(t.name.clone(), t.generics.clone());
                    }
                }
                match &t.kind {
                    TypeDefKind::Alias(ty) => {
                        let resolved = self.resolve_type(ty);
                        self.check_type_well_formed(&resolved, &format!("alias '{}'", t.name));
                        self.aliases.insert(t.name.clone(), resolved);
                        self.alias_spans.insert(t.name.clone(), t.meta.span);
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
                        // CK2: Build self_ty with generic args for proper substitution
                        let generic_args: Vec<Type> = t
                            .generics
                            .iter()
                            .map(|g| Type::Name(g.name.clone(), vec![]))
                            .collect();
                        let self_ty = Type::Name(t.name.clone(), generic_args);
                        for v in variants {
                            // CK3: Check constructor doesn't shadow existing function
                            if self.funcs.contains_key(&v.name) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0402,
                                    format!(
                                        "variant constructor '{}' shadows existing function '{}'",
                                        v.name, v.name
                                    ),
                                );
                            }
                            let ret = self_ty.clone();
                            let params = match &v.payload {
                                None => vec![],
                                Some(VariantPayload::Tuple(types)) => {
                                    types.iter().map(|ty| self.resolve_type(ty)).collect()
                                }
                                Some(VariantPayload::Record(fields)) => {
                                    fields.iter().map(|f| self.resolve_type(&f.ty)).collect()
                                }
                            };
                            for p in &params {
                                self.check_type_well_formed(
                                    p,
                                    &format!("variant '{}' of enum '{}'", v.name, t.name),
                                );
                            }
                            self.funcs.insert(v.name.clone(), (params, ret));
                        }
                    }
                    TypeDefKind::Record(fields) => {
                        for field in fields {
                            let field_ty = self.resolve_type(&field.ty);
                            self.check_type_well_formed(
                                &field_ty,
                                &format!("field '{}' of record '{}'", field.name, t.name),
                            );
                        }
                    }
                    TypeDefKind::Union(fields) => {
                        for field in fields {
                            let field_ty = self.resolve_type(&field.ty);
                            self.check_type_well_formed(
                                &field_ty,
                                &format!("field '{}' of union '{}'", field.name, t.name),
                            );
                        }
                    }
                }
                self.generic_scope
                    .truncate(self.generic_scope.len() - generic_names.len());
                if !allow_recursive {
                    self.types.insert(t.name.clone(), t.clone());
                    // Store generic parameters for type definitions
                    if !t.generics.is_empty() {
                        self.type_generics
                            .insert(t.name.clone(), t.generics.clone());
                    }
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
                    meta: AstNodeMeta::inherited(
                        actor.meta.span,
                        AstOrigin::Desugared("checker.actor_record_projection"),
                    ),
                    name: actor.name.clone(),
                    pub_: actor.pub_,
                    kind: TypeDefKind::Record(
                        actor
                            .fields
                            .iter()
                            .map(|f| Field {
                                meta: AstNodeMeta::inherited(
                                    f.meta.span,
                                    AstOrigin::Desugared("checker.actor_field_projection"),
                                ),
                                name: f.name.clone(),
                                ty: f.ty.clone(),
                            })
                            .collect(),
                    ),
                    generics: Vec::new(),
                    derives: Vec::new(),
                    attributes: Vec::new(),
                };
                self.types.insert(actor.name.clone(), actor_type_def);

                // Collect actor methods as functions
                for method in &actor.methods {
                    self.set_span(method.meta.span);
                    let qualified = format!("{}::{}", actor.name, method.name);
                    if self.funcs.contains_key(&qualified) {
                        self.emit_code(
                            crate::diagnostic::codes::E0402,
                            format!(
                                "duplicate function definition '{}' in actor '{}'",
                                method.name, actor.name
                            ),
                        );
                        return;
                    }
                    let generic_names: Vec<String> =
                        method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(generic_names.iter().cloned());
                    // Add implicit self parameter as first param
                    let self_type = Type::Name(actor.name.clone(), vec![]);
                    let has_explicit_self = method
                        .params
                        .first()
                        .is_some_and(|param| param.name == "self");
                    let mut params = if has_explicit_self {
                        Vec::new()
                    } else {
                        vec![self_type]
                    };
                    params.extend(method.params.iter().map(|p| self.resolve_type(&p.ty)));
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    for (i, p) in method.params.iter().enumerate() {
                        self.check_type_well_formed(
                            &params[i + usize::from(!has_explicit_self)],
                            &format!("parameter '{}' of actor method '{}'", p.name, method.name),
                        );
                    }
                    self.check_type_well_formed(
                        &ret,
                        &format!("return type of actor method '{}'", method.name),
                    );
                    self.generic_scope
                        .truncate(self.generic_scope.len() - generic_names.len());
                    self.funcs
                        .insert(format!("{}::{}", actor.name, method.name), (params, ret));
                }
            }
            Item::Cap(c) => {
                self.set_span(c.meta.span);
                if !self.declared_caps.insert(c.name.clone()) {
                    self.emit_code(
                        crate::diagnostic::codes::E0402,
                        format!("duplicate capability declaration '{}'", c.name),
                    );
                }
            }
            Item::Trait(trait_def) => {
                self.set_span(trait_def.meta.span);
                let method_names: Vec<String> =
                    trait_def.methods.iter().map(|m| m.name.clone()).collect();
                self.traits
                    .insert(trait_def.name.clone(), method_names.clone());
                let generic_names: Vec<String> =
                    trait_def.generics.iter().map(|g| g.name.clone()).collect();
                self.trait_generics
                    .insert(trait_def.name.clone(), generic_names.clone());
                // Push trait generics into scope so method signatures can reference them
                self.generic_scope.extend(generic_names.iter().cloned());
                // Store trait method signatures for argument validation
                for method in &trait_def.methods {
                    let params: Vec<Type> = method
                        .params
                        .iter()
                        .map(|p| self.resolve_type(&p.ty))
                        .collect();
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    self.trait_method_sigs
                        .insert((trait_def.name.clone(), method.name.clone()), (params, ret));
                }
                self.generic_scope
                    .truncate(self.generic_scope.len() - generic_names.len());
            }
            Item::Impl(impl_def) => {
                self.set_span(impl_def.meta.span);
                let method_names: Vec<String> =
                    impl_def.methods.iter().map(|m| m.name.clone()).collect();
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
                let impl_generic_names: Vec<String> =
                    impl_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope
                    .extend(impl_generic_names.iter().cloned());
                for method in &impl_def.methods {
                    self.set_span(method.meta.span);
                    let generic_names: Vec<String> =
                        method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(generic_names.iter().cloned());
                    let has_explicit_self = method
                        .params
                        .first()
                        .is_some_and(|param| param.name == "self");
                    let mut params = if has_explicit_self {
                        Vec::new()
                    } else {
                        vec![Type::Name(
                            impl_def.type_name.clone(),
                            impl_def.type_args.clone(),
                        )]
                    };
                    params.extend(method.params.iter().map(|p| self.resolve_type(&p.ty)));
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    for (i, p) in method.params.iter().enumerate() {
                        self.check_type_well_formed(
                            &params[i + usize::from(!has_explicit_self)],
                            &format!("parameter '{}' of impl method '{}'", p.name, method.name),
                        );
                    }
                    self.check_type_well_formed(
                        &ret,
                        &format!("return type of impl method '{}'", method.name),
                    );
                    self.generic_scope
                        .truncate(self.generic_scope.len() - generic_names.len());
                    // CK-C1: reject silent overwrite when two impls register the same method key.
                    let key = format!("{}_{}", impl_def.type_name, method.name);
                    if self.funcs.contains_key(&key) {
                        self.emit_code(
                            crate::diagnostic::codes::E0402,
                            format!(
                                "duplicate method '{}' for type '{}' (conflicting impl registrations)",
                                method.name, impl_def.type_name
                            ),
                        );
                    } else {
                        self.funcs.insert(key, (params, ret));
                    }
                }
                self.generic_scope
                    .truncate(self.generic_scope.len() - impl_generic_names.len());
            }
            Item::ExternBlock(block) => {
                self.set_span(block.meta.span);
                // Register extern functions for type checking
                for func in &block.funcs {
                    self.set_span(func.meta.span);
                    for param in &func.params {
                        self.set_span(param.meta.span);
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
                    let params: Vec<Type> = func
                        .params
                        .iter()
                        .map(|p| self.resolve_type(&p.ty))
                        .collect();
                    let ret = func
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    self.funcs.insert(func.name.clone(), (params, ret));
                }
            }
            Item::Const {
                meta,
                name,
                ty,
                value,
                ..
            } => {
                self.set_span(meta.span);
                // Infer the type of the constant value
                let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
                let value_ty = self.infer_expr(value, &mut scopes);
                let const_ty = if let Some(declared_ty) = ty {
                    self.resolve_type(declared_ty)
                } else {
                    value_ty
                };
                self.const_types.insert(name.clone(), const_ty);
            }
            Item::Flow(f) => {
                // Register states and transitions for type checking
                let qualified = format!("flow::{}", f.name);
                // FLOW-IDENTITY-001: register the root (first-declared) state for
                // state-unforgeability checking. Only root states may be constructed
                // via record literals outside transition bodies.
                if let Some(root) = f.states.first() {
                    self.flow_root_states
                        .insert(format!("{}::{}", qualified, root.name));
                    self.flow_root_states.insert(root.name.clone());
                }
                for state in &f.states {
                    let state_key = format!("{}::{}", qualified, state.name);
                    let payload_types = state
                        .payload
                        .as_ref()
                        .map(|fields| {
                            fields
                                .iter()
                                .map(|f| self.resolve_type(&f.ty))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    self.funcs.insert(
                        state_key,
                        (payload_types, Type::Name("unit".into(), vec![])),
                    );
                    // Register state payload as a Record type (both qualified and unqualified)
                    let type_name = format!("{}::{}", qualified, state.name);
                    self.types.entry(type_name.clone()).or_insert_with(|| {
                        let fields = state.payload.clone().unwrap_or_default();
                        TypeDef {
                            meta: AstNodeMeta::inherited(
                                state.meta.span,
                                AstOrigin::Desugared("checker.flow_state_type_projection"),
                            ),
                            name: type_name.clone(),
                            pub_: false,
                            kind: TypeDefKind::Record(fields),
                            generics: vec![],
                            derives: vec![],
                            attributes: vec![],
                        }
                    });
                    // Also register with unqualified name for use in transition bodies.
                    // CK-C2: never overwrite a user-declared type of the same name.
                    // T-H8: when two flows share an unqualified state name, verify
                    // payload compatibility to prevent silent type pollution.
                    if Self::is_builtin_type(&state.name) {
                        // skip
                    } else if let Some(existing) = self.types.get(&state.name) {
                        let is_flow_state = matches!(&existing.kind, TypeDefKind::Record(_));
                        if is_flow_state {
                            // T-H8: cross-flow unqualified name collision — payloads must match.
                            let current_fields = state.payload.as_deref().unwrap_or_default();
                            if let TypeDefKind::Record(existing_fields) = &existing.kind {
                                let compatible = current_fields.len() == existing_fields.len()
                                    && current_fields.iter().zip(existing_fields.iter()).all(
                                        |(a, b)| {
                                            a.name == b.name
                                                && types_compatible(
                                                    &self.resolve_type(&a.ty),
                                                    &self.resolve_type(&b.ty),
                                                )
                                        },
                                    );
                                if !compatible {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0402,
                                        format!(
                                            "flow state '{}' conflicts with another flow state of the same name; use the qualified name 'flow::<flow_name>::{}'",
                                            state.name, state.name
                                        ),
                                    );
                                } else {
                                    // FLOW-IDENTITY-001: nominal distinctness —
                                    // same-named states in different flows are never
                                    // the same type. The unqualified name is already
                                    // taken; this flow's state is only accessible via
                                    // the qualified name.
                                    self.emit_warning_code(
                                        crate::diagnostic::codes::E0422,
                                        format!(
                                            "flow state '{}' shares an unqualified name with another flow's state; \
                                             use the qualified name 'flow::<flow_name>::{}' to refer to this flow's state",
                                            state.name, state.name
                                        ),
                                    );
                                }
                            }
                        } else {
                            self.emit_code(
                                crate::diagnostic::codes::E0402,
                                format!(
                                    "flow state '{}' conflicts with existing type definition",
                                    state.name
                                ),
                            );
                        }
                    } else {
                        let fields2 = state.payload.clone().unwrap_or_default();
                        let td2 = TypeDef {
                            meta: AstNodeMeta::inherited(
                                state.meta.span,
                                AstOrigin::Desugared("checker.flow_state_type_projection"),
                            ),
                            name: state.name.clone(),
                            pub_: false,
                            kind: TypeDefKind::Record(fields2),
                            generics: vec![],
                            derives: vec![],
                            attributes: vec![],
                        };
                        self.types.insert(state.name.clone(), td2);
                    }
                    // CK-C5: system Fault sink requires a fixed payload shape
                    // (last_state/unexpected_event/snapshot/trace). Reject user-declared
                    // Fault that omits those fields (ensure_fault_state keeps user Fault
                    // as-is, which is incompatible with matrix recovery).
                    // System-injected Fault always has the required fields — no false positive.
                    if state.name == "Fault" {
                        let names: Vec<&str> = state
                            .payload
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .map(|fld| fld.name.as_str())
                            .collect();
                        let required = ["last_state", "unexpected_event", "snapshot", "trace"];
                        if !required.iter().all(|r| names.contains(r)) {
                            self.emit_code(
                                crate::diagnostic::codes::E0402,
                                format!(
                                    "user-declared state 'Fault' in flow '{}' is incompatible with the system Fault sink (required fields: last_state, unexpected_event, snapshot, trace)",
                                    f.name
                                ),
                            );
                        }
                    }
                }
                // Register transition functions.
                // Key includes from_state so overloads on different source
                // states coexist: `flow::Counter::inc::Zero`.
                // Signature: (from_state_payload, ...event_params) -> to_state
                // Multi-target transitions use the first target as the nominal
                // return type (call sites access common payload fields).
                // CK-H7: short keys only when a transition name is unique across
                // from_states — otherwise name-only lookup is ambiguous (HashMap
                // iteration order must not pick a "last" overload).
                let mut transition_name_counts: HashMap<&str, usize> = HashMap::new();
                for t in &f.transitions {
                    *transition_name_counts.entry(t.name.as_str()).or_insert(0) += 1;
                }
                for t in &f.transitions {
                    let t_key = format!("{}::{}::{}", qualified, t.name, t.from_state);
                    let mut params: Vec<Type> = Vec::new();
                    // First arg is the from-state payload (self)
                    params.push(Type::Name(t.from_state.clone(), vec![]));
                    for p in &t.params {
                        params.push(self.resolve_type(&p.ty));
                    }
                    let ret = if let Some(first) = t.to_states.first() {
                        Type::Name(first.clone(), vec![])
                    } else {
                        Type::Name("unit".into(), vec![])
                    };
                    self.funcs.insert(t_key, (params.clone(), ret.clone()));
                    if transition_name_counts.get(t.name.as_str()).copied() == Some(1) {
                        let short_key = format!("{}::{}", qualified, t.name);
                        self.funcs.insert(short_key, (params, ret));
                    }
                }
            }
            Item::Protocol(p) => {
                let qualified = format!("proto::{}", p.name);
                // Always register a protocol marker so empty-state protocols
                // (no payload fields) still resolve under `impl ProtocolName`.
                // Existence check uses `types` keys with prefix `proto::{name}`.
                if !self.types.contains_key(&qualified) {
                    let marker = TypeDef {
                        meta: AstNodeMeta::inherited(
                            p.meta.span,
                            AstOrigin::Desugared("checker.protocol_marker_projection"),
                        ),
                        name: qualified.clone(),
                        pub_: false,
                        kind: TypeDefKind::Record(vec![]),
                        generics: vec![],
                        derives: vec![],
                        attributes: vec![],
                    };
                    self.types.insert(qualified.clone(), marker);
                }
                for state in &p.states {
                    let state_key = format!("{}::{}", qualified, state.name);
                    self.funcs
                        .insert(state_key, (Vec::new(), Type::Name("unit".into(), vec![])));
                    // Register every protocol state as a (possibly empty) record type.
                    let type_name = format!("{}::{}", qualified, state.name);
                    self.types.entry(type_name.clone()).or_insert_with(|| {
                        let fields = match &state.payload_type {
                            Some(payload_ty) => vec![Field {
                                meta: AstNodeMeta::inherited(
                                    state.meta.span,
                                    AstOrigin::Desugared(
                                        "checker.protocol_payload_field_projection",
                                    ),
                                ),
                                name: state
                                    .payload_name
                                    .clone()
                                    .unwrap_or_else(|| "value".to_string()),
                                ty: payload_ty.clone(),
                            }],
                            None => vec![],
                        };
                        TypeDef {
                            meta: AstNodeMeta::inherited(
                                state.meta.span,
                                AstOrigin::Desugared("checker.protocol_state_type_projection"),
                            ),
                            name: type_name.clone(),
                            pub_: false,
                            kind: TypeDefKind::Record(fields),
                            generics: vec![],
                            derives: vec![],
                            attributes: vec![],
                        }
                    });
                }
            }
            Item::Session(s) => {
                // Register session type for order checking / dual resolution.
                if self.session_types.contains_key(&s.name) {
                    // duplicate handled in check_item
                } else {
                    self.session_types.insert(s.name.clone(), s.body.clone());
                }
                // Also expose SessionChan marker type so SessionChan<S> is well-formed.
                if !self.types.contains_key("SessionChan") {
                    let session_marker_meta = AstNodeMeta::synthetic(AstOrigin::RuntimeSystem(
                        "checker.session_channel_marker",
                    ));
                    let td = TypeDef {
                        meta: session_marker_meta,
                        name: "SessionChan".to_string(),
                        pub_: false,
                        kind: TypeDefKind::Record(vec![]),
                        generics: vec![GenericParam {
                            meta: session_marker_meta,
                            name: "S".to_string(),
                            bounds: vec![],
                        }],
                        derives: vec![],
                        attributes: vec![],
                    };
                    self.types.insert("SessionChan".to_string(), td);
                }
            }
        }
    }
    pub(crate) fn check_item(&mut self, item: &Item) {
        self.set_span(Self::item_span(item));
        match item {
            Item::Func(f) => {
                self.set_span(f.meta.span);
                self.check_func(f)
            }
            Item::Module(m) => {
                self.set_span(m.meta.span);
                self.module_path.push(m.name.clone());
                for inner in &m.items {
                    self.check_item(inner);
                }
                self.module_path.pop();
            }
            Item::Actor(actor) => {
                self.set_span(actor.meta.span);
                // Check actor fields
                for field in &actor.fields {
                    self.set_span(field.meta.span);
                    let field_ty = self.resolve_type(&field.ty);
                    // Validate field type is well-formed
                    self.check_type_well_formed(
                        &field_ty,
                        &format!("actor field '{}'", field.name),
                    );
                    // Check field initialization if present
                    if let Some(init) = &field.init {
                        let init_ty = self.infer_expr(init, &mut vec![HashMap::new()]);
                        // CK-H3: unify so TypeVars / Option payloads resolve.
                        if self.unification.unify(&field_ty, &init_ty).is_err() {
                            self.emit_code(
                                crate::diagnostic::codes::E0209,
                                format!(
                                "actor field '{}' initializer type {} does not match field type {}",
                                field.name,
                                fmt_type(&init_ty),
                                fmt_type(&field_ty)
                            ),
                            );
                        }
                    }
                }
                // Check actor methods
                for method in &actor.methods {
                    self.set_span(method.meta.span);
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
                    let actor_name = if self.module_path.is_empty() {
                        actor.name.clone()
                    } else {
                        format!("{}::{}", self.module_path.join("::"), actor.name)
                    };
                    let method_owner =
                        crate::core::NodeId(format!("function:{}::{}", actor_name, method.name));
                    let previous_owner = self.begin_callable(method_owner.clone());
                    self.unification.reset();
                    self.begin_expression_type_capture(method_owner);
                    let implicit_return =
                        self.check_block_with_implicit_return(&method.body, &ret, &mut scopes);
                    self.check_method_implicit_return(
                        &format!("actor '{}::{}'", actor.name, method.name),
                        &ret,
                        implicit_return,
                    );
                    self.finish_expression_type_capture();
                    self.end_callable(previous_owner);
                    self.var_scopes.pop();
                }
            }
            Item::Type(type_def) => {
                self.set_span(type_def.meta.span);
            }
            Item::Cap(cap) => self.set_span(cap.meta.span),
            Item::Trait(trait_def) => {
                self.set_span(trait_def.meta.span);
                // Check that all trait method types are well-formed
                let generic_names: Vec<String> =
                    trait_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope.extend(generic_names.iter().cloned());
                for method in &trait_def.methods {
                    let method_generic_names: Vec<String> =
                        method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope
                        .extend(method_generic_names.iter().cloned());
                    for param in &method.params {
                        let resolved = self.resolve_type(&param.ty);
                        self.check_type_well_formed(
                            &resolved,
                            &format!("trait '{}' method '{}'", trait_def.name, method.name),
                        );
                    }
                    if let Some(ret) = &method.ret {
                        let resolved = self.resolve_type(ret);
                        self.check_type_well_formed(
                            &resolved,
                            &format!("trait '{}' method '{}' return", trait_def.name, method.name),
                        );
                    }
                    self.generic_scope
                        .truncate(self.generic_scope.len() - method_generic_names.len());
                }
                self.generic_scope
                    .truncate(self.generic_scope.len() - generic_names.len());
            }
            Item::Impl(impl_def) => {
                self.set_span(impl_def.meta.span);
                // Check that the trait exists
                if !self.traits.contains_key(&impl_def.trait_name) {
                    self.emit_code(
                        crate::diagnostic::codes::E0406,
                        format!("undefined trait '{}'", impl_def.trait_name),
                    );
                }
                // Check that the type exists
                if !self.types.contains_key(&impl_def.type_name)
                    && !Self::is_builtin_type(&impl_def.type_name)
                {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0407,
                            format!("undefined type '{}'", impl_def.type_name),
                            self.diagnostic_span(),
                        ).with_help("types must be defined before use — check the type name spelling or add a 'type' declaration")
                    );
                }
                // Check that all required trait methods are implemented
                if let Some(required_methods) = self.traits.get(&impl_def.trait_name).cloned() {
                    let implemented: Vec<String> =
                        impl_def.methods.iter().map(|m| m.name.clone()).collect();
                    for required in &required_methods {
                        if !implemented.contains(required) {
                            self.emit_code(
                                crate::diagnostic::codes::E0252,
                                format!(
                                    "missing method '{}' in impl of trait '{}' for '{}'",
                                    required, impl_def.trait_name, impl_def.type_name
                                ),
                            );
                        }
                    }
                    // CK-H5: verify impl method signatures match the trait.
                    for method in &impl_def.methods {
                        if let Some((trait_params, trait_ret)) = self
                            .trait_method_sigs
                            .get(&(impl_def.trait_name.clone(), method.name.clone()))
                            .cloned()
                        {
                            let impl_params: Vec<Type> = method
                                .params
                                .iter()
                                .map(|p| self.resolve_type(&p.ty))
                                .collect();
                            let impl_ret = method
                                .ret
                                .as_ref()
                                .map(|t| self.resolve_type(t))
                                .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                            // Trait params usually exclude `self`; compare trailing params.
                            let trait_user = if trait_params.len() == impl_params.len() + 1 {
                                &trait_params[1..]
                            } else {
                                trait_params.as_slice()
                            };
                            if trait_user.len() != impl_params.len() {
                                self.emit_code(
                                    crate::diagnostic::codes::E0252,
                                    format!(
                                        "method '{}' in impl of '{}' for '{}' has {} parameters, trait requires {}",
                                        method.name,
                                        impl_def.trait_name,
                                        impl_def.type_name,
                                        impl_params.len(),
                                        trait_user.len()
                                    ),
                                );
                            } else {
                                for (i, (tp, ip)) in
                                    trait_user.iter().zip(impl_params.iter()).enumerate()
                                {
                                    if self.unification.unify(tp, ip).is_err() {
                                        self.emit_code(
                                            crate::diagnostic::codes::E0252,
                                            format!(
                                                "method '{}' param {} type {} does not match trait {} (expected {})",
                                                method.name,
                                                i + 1,
                                                fmt_type(ip),
                                                impl_def.trait_name,
                                                fmt_type(tp)
                                            ),
                                        );
                                    }
                                }
                            }
                            if self.unification.unify(&trait_ret, &impl_ret).is_err() {
                                self.emit_code(
                                    crate::diagnostic::codes::E0252,
                                    format!(
                                        "method '{}' return type {} does not match trait {} (expected {})",
                                        method.name,
                                        fmt_type(&impl_ret),
                                        impl_def.trait_name,
                                        fmt_type(&trait_ret)
                                    ),
                                );
                            }
                        }
                    }
                }
                // Check impl method bodies with self bound to the implementing type
                let impl_generic_names: Vec<String> =
                    impl_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope
                    .extend(impl_generic_names.iter().cloned());
                let impl_qualified_name = if self.module_path.is_empty() {
                    format!("{}:for:{}", impl_def.trait_name, impl_def.type_name)
                } else {
                    format!(
                        "{}::{}:for:{}",
                        self.module_path.join("::"),
                        impl_def.trait_name,
                        impl_def.type_name
                    )
                };
                for method in &impl_def.methods {
                    self.set_span(method.meta.span);
                    let method_generic_names: Vec<String> =
                        method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope
                        .extend(method_generic_names.iter().cloned());
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
                    // Bind self with the implementing type
                    scopes[0].insert(
                        "self".to_string(),
                        Type::Name(impl_def.type_name.clone(), impl_def.type_args.clone()),
                    );
                    for p in &method.params {
                        let ty = self.resolve_type(&p.ty);
                        scopes[0].insert(p.name.clone(), ty);
                    }
                    self.var_scopes.push(HashMap::new());
                    let method_owner =
                        crate::core::resolved::impl_method_owner(&impl_qualified_name, method);
                    let previous_owner = self.begin_callable(method_owner.clone());
                    self.unification.reset();
                    self.begin_expression_type_capture(method_owner);
                    let implicit_return =
                        self.check_block_with_implicit_return(&method.body, &ret, &mut scopes);
                    self.check_method_implicit_return(
                        &format!(
                            "method '{}' in impl of '{}' for '{}'",
                            method.name, impl_def.trait_name, impl_def.type_name
                        ),
                        &ret,
                        implicit_return,
                    );
                    self.finish_expression_type_capture();
                    self.end_callable(previous_owner);
                    self.var_scopes.pop();
                    self.generic_scope
                        .truncate(self.generic_scope.len() - method_generic_names.len());
                }
                self.generic_scope
                    .truncate(self.generic_scope.len() - impl_generic_names.len());
            }
            Item::ExternBlock(block) => {
                self.set_span(block.meta.span);
                // CK-H4: validate return types in the check pass (params already
                // validated during collect). Skip body (extern has no body).
                if !block.unsafe_ {
                    for func in &block.funcs {
                        self.set_span(func.meta.span);
                        if let Some(ret_ty) = &func.ret {
                            let resolved = self.resolve_type(ret_ty);
                            if !self.is_valid_extern_type(&resolved, false) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0231,
                                    format!(
                                        "extern function '{}' return type '{}' is not allowed across the C ABI boundary",
                                        func.name,
                                        fmt_type(&resolved)
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            Item::Const {
                meta,
                name,
                ty,
                value,
                ..
            } => {
                self.set_span(meta.span);
                let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
                let value_ty = self.infer_expr(value, &mut scopes);
                let const_ty = if let Some(declared_ty) = ty {
                    let resolved = self.resolve_type(declared_ty);
                    // CK-H10: unify (not same_type) for TypeVar resolution.
                    if self.unification.unify(&resolved, &value_ty).is_err() {
                        self.emit_code(
                            crate::diagnostic::codes::E0209,
                            format!(
                                "const '{}' declared type {} does not match value type {}",
                                name,
                                fmt_type(&resolved),
                                fmt_type(&value_ty)
                            ),
                        );
                    }
                    self.unification.resolve(&resolved)
                } else {
                    value_ty
                };
                // Register const type so that later items can reference it.
                // infer_item already does this; check_item must too.
                self.const_types.insert(name.clone(), const_ty);
            }
            Item::Flow(f) => {
                self.set_span(f.meta.span);
                // Check state name uniqueness
                let mut seen_states: std::collections::HashSet<&str> =
                    std::collections::HashSet::new();
                for s in &f.states {
                    if !seen_states.insert(s.name.as_str()) {
                        self.emit_code(
                            crate::diagnostic::codes::E0402,
                            format!("duplicate state '{}' in flow '{}'", s.name, f.name),
                        );
                    }
                    // Validate payload types are well-formed
                    if let Some(fields) = &s.payload {
                        for field in fields {
                            let resolved = self.resolve_type(&field.ty);
                            self.check_type_well_formed(
                                &resolved,
                                &format!(
                                    "state '{}' payload field '{}' in flow '{}'",
                                    s.name, field.name, f.name
                                ),
                            );
                        }
                    }
                }
                // Check transition uniqueness by (name, from_state) — same event
                // name may overload across different source states.
                let mut seen_transitions: std::collections::HashSet<(&str, &str)> =
                    std::collections::HashSet::new();
                for t in &f.transitions {
                    if !seen_transitions.insert((t.name.as_str(), t.from_state.as_str())) {
                        self.emit_code(
                            crate::diagnostic::codes::E0402,
                            format!(
                                "duplicate transition '{}({})' in flow '{}'",
                                t.name, t.from_state, f.name
                            ),
                        );
                    }
                }
                // T-H13: verify overloaded event params are consistent across
                // different source states (fallback matrix uses first-entry params).
                let mut event_params: HashMap<&str, &[Param]> = HashMap::new();
                for t in &f.transitions {
                    if t.name == "reset" || t.name == "recover" || t.name == "peer_fault" {
                        continue;
                    }
                    if let Some(existing) = event_params.get(&t.name.as_str()) {
                        if existing.len() != t.params.len()
                            || existing
                                .iter()
                                .zip(t.params.iter())
                                .any(|(a, b)| a.ty != b.ty)
                        {
                            self.emit_code(
                                crate::diagnostic::codes::E0402,
                                format!(
                                    "event '{}' in flow '{}' has inconsistent param types across overloads; all from-states must use the same param shape",
                                    t.name, f.name
                                ),
                            );
                        }
                    } else {
                        event_params.insert(&t.name, &t.params);
                    }
                }
                // Validate that all referenced states exist
                let state_names: Vec<&str> = f.states.iter().map(|s| s.name.as_str()).collect();
                for t in &f.transitions {
                    if !state_names.contains(&t.from_state.as_str()) && t.from_state != "Fault" {
                        self.emit_code(
                            crate::diagnostic::codes::E0404,
                            format!("state '{}' referenced in transition '{}' is not defined in flow '{}'",
                                    t.from_state, t.name, f.name),
                        );
                    }
                    for to_state in &t.to_states {
                        if to_state != "Fault" && !state_names.contains(&to_state.as_str()) {
                            self.emit_code(
                                crate::diagnostic::codes::E0404,
                                format!("target state '{}' in transition '{}' is not defined in flow '{}'",
                                        to_state, t.name, f.name),
                            );
                        }
                    }
                    // Codegen currently represents a multi-target result with one
                    // nominal LLVM struct.  Permit this only when every target has
                    // the same ordered field types; otherwise choosing the first
                    // target would silently reinterpret a different layout (M6).
                    if t.to_states.len() > 1 {
                        let target_layouts = t.to_states.iter().filter_map(|target| {
                            f.states
                                .iter()
                                .find(|state| state.name == *target)
                                .map(|state| {
                                    state
                                        .payload
                                        .as_deref()
                                        .unwrap_or_default()
                                        .iter()
                                        .map(|field| self.resolve_type(&field.ty))
                                        .collect::<Vec<_>>()
                                })
                        });
                        let mut target_layouts = target_layouts;
                        if let Some(first_layout) = target_layouts.next() {
                            if target_layouts.any(|layout| layout != first_layout) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0419,
                                    format!(
                                        "multi-target transition '{}({})' in flow '{}' has incompatible target payload layouts; use states with the same ordered field types or split the transition",
                                        t.name, t.from_state, f.name
                                    ),
                                );
                            }
                        }
                    }
                    // Type-check transition body with self in scope
                    if let Some(body) = &t.body {
                        if !t.is_fallback && !self.block_returns_on_all_paths(body) {
                            self.emit_code(
                                crate::diagnostic::codes::E0255,
                                format!(
                                    "transition '{}({})' in flow '{}' does not return a target state on all paths",
                                    t.name, t.from_state, f.name
                                ),
                            );
                        }
                        let from_payload = f
                            .states
                            .iter()
                            .find(|s| s.name == t.from_state)
                            .and_then(|s| s.payload.as_ref());
                        let mut scopes: Vec<std::collections::HashMap<String, Type>> =
                            vec![std::collections::HashMap::new()];
                        // CK-H9: self uses the unqualified state name so it
                        // unifies with bare record literals (Zero { … }) and
                        // with Type::Name(state) registered under short names.
                        if from_payload.is_some() {
                            let self_ty = Type::Name(t.from_state.clone(), vec![]);
                            scopes[0].insert("self".to_string(), self_ty);
                        } else {
                            // No payload: self is unit
                            scopes[0].insert("self".to_string(), Type::Name("unit".into(), vec![]));
                        }
                        // Add transition params to scope
                        for p in &t.params {
                            let resolved = self.resolve_type(&p.ty);
                            self.check_type_well_formed(
                                &resolved,
                                &format!(
                                    "transition '{}' param '{}' in flow '{}'",
                                    t.name, p.name, f.name
                                ),
                            );
                            scopes[0].insert(p.name.clone(), resolved);
                        }
                        let prev_ret = self.current_ret.take();
                        let prev_flow_targets = std::mem::take(&mut self.flow_return_targets);
                        let ret_type: Type = if t.to_states.len() == 1 {
                            // Use unqualified state name since record literals produce bare names
                            Type::Name(t.to_states[0].clone(), vec![])
                        } else {
                            // Multi-target: validate each return against allowed types
                            let mut allowed = Vec::new();
                            for ts in &t.to_states {
                                allowed.push(Type::Name(ts.clone(), vec![]));
                            }
                            self.flow_return_targets = allowed;
                            // Use unit as ret to suppress per-return unification errors
                            Type::Name("unit".into(), vec![])
                        };
                        self.current_ret = Some(ret_type.clone());
                        self.var_scopes.push(std::collections::HashMap::new());
                        let flow_name = if self.module_path.is_empty() {
                            f.name.clone()
                        } else {
                            format!("{}::{}", self.module_path.join("::"), f.name)
                        };
                        let transition_owner = crate::core::NodeId(format!(
                            "transition:{}::{}::{}",
                            flow_name, t.name, t.from_state
                        ));
                        let previous_owner = self.begin_callable(transition_owner.clone());
                        let capture_typed_body = matches!(t.meta.origin, AstOrigin::User);
                        if capture_typed_body {
                            self.begin_expression_type_capture(transition_owner);
                        }
                        // Type-check the body as a block
                        self.check_block(body, &ret_type, &mut scopes);
                        if capture_typed_body {
                            self.finish_expression_type_capture();
                        }
                        self.end_callable(previous_owner);
                        self.var_scopes.pop();
                        self.current_ret = prev_ret;
                        self.flow_return_targets = prev_flow_targets;
                    }
                }
                // Check impl_protocols references exist and validate conformance
                let flow_state_names: Vec<&str> =
                    f.states.iter().map(|s| s.name.as_str()).collect();
                for proto_name in &f.impl_protocols {
                    let proto_key = format!("proto::{}", proto_name);
                    if !self.types.iter().any(|(k, _)| k.starts_with(&proto_key)) {
                        self.emit_code(
                            crate::diagnostic::codes::E0406,
                            format!(
                                "protocol '{}' referenced in flow '{}' is not defined",
                                proto_name, f.name
                            ),
                        );
                        continue;
                    }
                    // Look up the protocol definition
                    let proto = self.file.items.iter().find_map(|item| {
                        if let Item::Protocol(p) = item {
                            if p.name == *proto_name {
                                return Some(p);
                            }
                        }
                        None
                    });
                    let Some(proto) = proto else { continue };
                    // 1. Verify flow defines all protocol states
                    for ps in &proto.states {
                        if !flow_state_names.contains(&ps.name.as_str()) {
                            self.emit_code(
                                crate::diagnostic::codes::E0404,
                                format!(
                                    "flow '{}' implements protocol '{}' but is missing required state '{}'",
                                    f.name, proto_name, ps.name
                                ),
                            );
                            continue;
                        }
                        // Check payload compatibility: protocol state has payload_type,
                        // flow state must have a matching field
                        if let (Some(proto_payload_name), Some(proto_payload_ty)) =
                            (&ps.payload_name, &ps.payload_type)
                        {
                            // L7: missing flow state for protocol state → diagnostic, not ICE.
                            let Some(flow_state) = f.states.iter().find(|s| s.name == ps.name)
                            else {
                                self.emit_code(
                                    crate::diagnostic::codes::E0412,
                                    format!(
                                        "protocol state '{}' not found on flow '{}' during payload check",
                                        ps.name, f.name
                                    ),
                                );
                                continue;
                            };
                            // Use an isolated checked-unification query so failed
                            // candidates cannot mutate the checker's constraint table.
                            let expected_ty = self.resolve_type(proto_payload_ty);
                            let has_field = flow_state
                                .payload
                                .as_ref()
                                .map(|fields| {
                                    fields.iter().any(|field| {
                                        let field_ty = self.resolve_type(&field.ty);
                                        field.name == *proto_payload_name
                                            && types_compatible(&field_ty, &expected_ty)
                                    })
                                })
                                .unwrap_or(false);
                            if !has_field {
                                self.emit_code(
                                    crate::diagnostic::codes::E0209,
                                    format!(
                                        "flow '{}' state '{}' must have protocol payload field '{}: {}'",
                                        f.name, ps.name, proto_payload_name,
                                        crate::core::fmt_type(&self.resolve_type(proto_payload_ty))
                                    ),
                                );
                            }
                        }
                    }
                    // 2. Verify flow defines all protocol transitions (topology cover).
                    // Multi-target transitions cover a protocol edge if the required
                    // to_state is among the declared targets (conservative projection).
                    for pt in &proto.transitions {
                        let has_transition = f.transitions.iter().any(|t| {
                            t.name == pt.name
                                && t.from_state == pt.from_state
                                && t.to_states.contains(&pt.to_state)
                        });
                        if !has_transition {
                            self.emit_code(
                                crate::diagnostic::codes::E0404,
                                format!(
                                    "flow '{}' implements protocol '{}' but is missing required transition '{}({}) -> {}'",
                                    f.name, proto_name, pt.name, pt.from_state, pt.to_state
                                ),
                            );
                        }
                    }
                    // 3. v0.29.36 / T-H15: Payload covariance / invariance rules.
                    //    - view (default): covariant width — flow may have extra fields
                    //    - mutate (Type::RefMut or name "mutate T"): invariant — flow
                    //      payload field types must exactly match protocol payload type
                    for ps in &proto.states {
                        let Some(proto_ty) = ps.payload_type.as_ref() else {
                            continue;
                        };
                        let is_mutate =
                            matches!(proto_ty.unlocated(), crate::ast::Type::RefMut(_, _))
                                || matches!(
                                    proto_ty.unlocated(),
                                    crate::ast::Type::Name(n, _) if n == "mutate"
                                );
                        if !is_mutate {
                            continue;
                        }
                        let Some(fs) = f.states.iter().find(|s| s.name == ps.name) else {
                            continue;
                        };
                        // Exact payload type match: if protocol names a payload field,
                        // flow field must unify exactly (no extra fields for mutate).
                        if let (Some(pname), Some(fields)) = (&ps.payload_name, &fs.payload) {
                            let expected = self.resolve_type(match proto_ty.unlocated() {
                                crate::ast::Type::RefMut(_, inner) => inner.as_ref(),
                                other => other,
                            });
                            let has_exact = fields.iter().any(|field| {
                                field.name == *pname
                                    && self
                                        .unification
                                        .unify(&self.resolve_type(&field.ty), &expected)
                                        .is_ok()
                            });
                            // Invariant: no extra fields beyond protocol payload.
                            if fields.len() != 1 || !has_exact {
                                self.emit_code(
                                    crate::diagnostic::codes::E0209,
                                    format!(
                                        "protocol mutate payload is invariant: flow '{}' state '{}' must match protocol payload exactly (no extra fields)",
                                        f.name, ps.name
                                    ),
                                );
                            }
                        }
                    }
                    //
                    // 4. v0.29.36: Conservative projection (E0418).
                    //    If a flow state payload contains a subflow (nested flow
                    //    state record), the projection to the flat protocol topology
                    //    must be conservative: the subflow's transitions must not
                    //    conflict with the protocol's transition set.
                    //    We check: if a flow state's payload field type matches
                    //    another flow's state name (subflow nesting), the protocol
                    //    must not declare transitions that would require observing
                    //    the inner subflow's state.
                    for ps in &proto.states {
                        let flow_state = f.states.iter().find(|s| s.name == ps.name);
                        if let Some(fs) = flow_state {
                            if let Some(ref payload) = fs.payload {
                                for field in payload {
                                    let field_ty_name = match field.ty.unlocated() {
                                        crate::ast::Type::Name(n, _) => n.clone(),
                                        _ => continue,
                                    };
                                    // Check if this field type is a subflow state
                                    // (i.e., another flow's state record name)
                                    let is_subflow_state = self.file.items.iter().any(|item| {
                                        if let crate::ast::Item::Flow(other_flow) = item {
                                            other_flow
                                                .states
                                                .iter()
                                                .any(|s| s.name == field_ty_name)
                                                && other_flow.name != f.name
                                        } else {
                                            false
                                        }
                                    });
                                    if is_subflow_state {
                                        // Conservative projection: subflow state in
                                        // protocol payload → E0418 if protocol has
                                        // transitions that target this state
                                        let proto_targets_this = proto
                                            .transitions
                                            .iter()
                                            .any(|pt| pt.to_state == field_ty_name);
                                        if proto_targets_this {
                                            self.emit_code(
                                                crate::diagnostic::codes::E0418,
                                                format!(
                                                    "conservative projection failure: flow '{}' state '{}' nests subflow state '{}' which is also a protocol transition target — flat projection is ambiguous",
                                                    f.name, ps.name, field_ty_name
                                                ),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // State machine validation: reachability and completeness.
                // Only count user-written transitions — auto-injected Fault
                // fallbacks would otherwise make every state look fully wired.
                let mut targeted_by: std::collections::HashSet<&str> =
                    std::collections::HashSet::new();
                let mut has_outgoing: std::collections::HashSet<&str> =
                    std::collections::HashSet::new();
                for t in &f.transitions {
                    if t.is_fallback {
                        continue;
                    }
                    for to_state in &t.to_states {
                        if to_state != "Fault" {
                            targeted_by.insert(to_state.as_str());
                        }
                    }
                    if t.from_state != "Fault" {
                        has_outgoing.insert(t.from_state.as_str());
                    }
                }
                // Warn about states with no incoming transitions (unreachable from other
                // states). The first declared state is implicitly the initial state.
                // Fault is the system sink; it may only be reached via fallbacks and is
                // never warned as unreachable.
                for s in &f.states {
                    if s.name == "Fault" {
                        continue;
                    }
                    if !targeted_by.contains(s.name.as_str()) {
                        // Skip the first state — it's the initial entry state
                        let is_first = f
                            .states
                            .first()
                            .map(|first| first.name == s.name)
                            .unwrap_or(false);
                        if !is_first {
                            self.warnings.push(
                                crate::diagnostic::Diagnostic::warning_code(
                                    crate::diagnostic::codes::W0400,
                                    format!(
                                        "state '{}' in flow '{}' is unreachable (no transition targets to it)",
                                        s.name, f.name
                                    ),
                                    s.meta.span,
                                )
                            );
                        }
                    }
                }
                // Warn about states with no outgoing transitions (terminal but not declared
                // as terminal — may indicate incomplete flow definition).
                // Fault is the absorbing sink (transfer-matrix auto-completion); skip it.
                for s in &f.states {
                    if s.name == "Fault" {
                        continue;
                    }
                    if !has_outgoing.contains(s.name.as_str()) {
                        self.warnings.push(
                            crate::diagnostic::Diagnostic::warning_code(
                                crate::diagnostic::codes::W0401,
                                format!(
                                    "state '{}' in flow '{}' has no outgoing transitions (terminal state)",
                                    s.name, f.name
                                ),
                                s.meta.span,
                            )
                        );
                    }
                }
            }
            Item::Protocol(p) => {
                self.set_span(p.meta.span);
                // Check state name uniqueness
                let mut seen_states: std::collections::HashSet<&str> =
                    std::collections::HashSet::new();
                for s in &p.states {
                    if !seen_states.insert(s.name.as_str()) {
                        self.emit_code(
                            crate::diagnostic::codes::E0402,
                            format!("duplicate state '{}' in protocol '{}'", s.name, p.name),
                        );
                    }
                    // Validate payload types are well-formed
                    if let Some(ref payload_ty) = s.payload_type {
                        let resolved = self.resolve_type(payload_ty);
                        self.check_type_well_formed(
                            &resolved,
                            &format!("state '{}' payload type in protocol '{}'", s.name, p.name),
                        );
                        // v0.29.18 flatness: protocol payloads must not nest other
                        // protocol states (session subtyping is undecidable on nested
                        // pushdown automata). Reject Type::Name matching a peer state.
                        if let Type::Name(n, _) = resolved.unlocated() {
                            if p.states.iter().any(|ps| ps.name == *n) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0412,
                                    format!(
                                        "protocol '{}' must be flat: state '{}' payload type '{}' nests protocol state '{}' (nested subflow topology is not allowed in protocols)",
                                        p.name, s.name, n, n
                                    ),
                                );
                            }
                        }
                    }
                }
                // Uniqueness by (name, from_state) — same event may overload across sources.
                let proto_state_names: Vec<&str> =
                    p.states.iter().map(|s| s.name.as_str()).collect();
                let mut seen_transitions: std::collections::HashSet<(&str, &str)> =
                    std::collections::HashSet::new();
                for t in &p.transitions {
                    if !seen_transitions.insert((t.name.as_str(), t.from_state.as_str())) {
                        self.emit_code(
                            crate::diagnostic::codes::E0402,
                            format!(
                                "duplicate transition '{}({})' in protocol '{}'",
                                t.name, t.from_state, p.name
                            ),
                        );
                    }
                    if !proto_state_names.contains(&t.from_state.as_str()) {
                        self.emit_code(
                            crate::diagnostic::codes::E0404,
                            format!("state '{}' referenced in protocol transition '{}' is not defined in protocol '{}'",
                                    t.from_state, t.name, p.name),
                        );
                    }
                    if !proto_state_names.contains(&t.to_state.as_str()) {
                        self.emit_code(
                            crate::diagnostic::codes::E0404,
                            format!("target state '{}' in protocol transition '{}' is not defined in protocol '{}'",
                                    t.to_state, t.name, p.name),
                        );
                    }
                }
            }
            Item::Session(s) => {
                self.set_span(s.meta.span);
                // Duplicate session names
                let count = self
                    .file
                    .items
                    .iter()
                    .filter(|i| matches!(i, Item::Session(o) if o.name == s.name))
                    .count();
                if count > 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0402,
                        format!("duplicate session type '{}'", s.name),
                    );
                }
                // Resolve body; unknown names are errors.
                self.check_session_type_wf(&s.body, &s.name);
            }
        }
    }

    fn check_method_implicit_return(
        &mut self,
        context: &str,
        declared: &Type,
        implicit: Option<Type>,
    ) {
        let Some(actual) = implicit else { return };
        let actual = self.unification.resolve(&actual);
        let actual = match actual.into_unlocated() {
            Type::Shared(inner) | Type::LocalShared(inner) | Type::CShared(inner) => *inner,
            other => other,
        };
        if !is_numeric_coercion(declared, &actual)
            && self.unification.unify(declared, &actual).is_err()
            && !matches!(declared.unlocated(), Type::Name(name, _) if name == "unit")
        {
            self.emit_code(
                crate::diagnostic::codes::E0207,
                format!(
                    "implicit return in {}: expected {}, found {}",
                    context,
                    fmt_type(declared),
                    fmt_type(&actual)
                ),
            );
        }
    }

    /// Well-formedness for a session type expression (v0.29.19).
    fn check_session_type_wf(&mut self, st: &crate::ast::SessionType, context: &str) {
        use crate::ast::SessionType;
        match st.unlocated() {
            SessionType::Send(t, cont) | SessionType::Recv(t, cont) => {
                let resolved = self.resolve_type(t);
                self.check_type_well_formed(
                    &resolved,
                    &format!("payload type in session '{}'", context),
                );
                self.check_session_type_wf(cont, context);
            }
            SessionType::Dual(inner) => self.check_session_type_wf(inner, context),
            SessionType::End => {}
            SessionType::Name(n) => {
                if !self.session_types.contains_key(n) {
                    self.emit_code(
                        crate::diagnostic::codes::E0413,
                        format!(
                            "unknown session type '{}' referenced in session '{}'",
                            n, context
                        ),
                    );
                }
            }
            SessionType::Located { .. } => {
                unreachable!("SessionType::unlocated returned Located")
            }
        }
    }
}
