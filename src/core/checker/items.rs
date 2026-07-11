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
        // ExecResult { exit_code: i32, stdout: string, stderr: string }
        if !self.types.contains_key("ExecResult") {
            let td = TypeDef {
                name: "ExecResult".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        name: "exit_code".to_string(),
                        ty: Type::Name("i32".to_string(), vec![]),
                    },
                    Field {
                        name: "stdout".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
                    },
                    Field {
                        name: "stderr".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
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
                name: "StatResult".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        name: "size".to_string(),
                        ty: Type::Name("i64".to_string(), vec![]),
                    },
                    Field {
                        name: "modified".to_string(),
                        ty: Type::Name("i64".to_string(), vec![]),
                    },
                    Field {
                        name: "is_file".to_string(),
                        ty: Type::Name("bool".to_string(), vec![]),
                    },
                    Field {
                        name: "is_dir".to_string(),
                        ty: Type::Name("bool".to_string(), vec![]),
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
                name: "PeerFault".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        name: "peer_id".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
                    },
                    Field {
                        name: "reason".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
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
                name: "SystemTrace".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        name: "last_state_name".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
                    },
                    Field {
                        name: "unexpected_event".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
                    },
                    Field {
                        name: "snapshot".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
                    },
                    Field {
                        name: "memory_dump".to_string(),
                        ty: Type::Name("MemoryDump".to_string(), vec![]),
                    },
                    Field {
                        name: "panic_payload".to_string(),
                        ty: Type::Name("PanicPayload".to_string(), vec![]),
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
                name: "PanicPayload".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        name: "error_type".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
                    },
                    Field {
                        name: "file".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
                    },
                    Field {
                        name: "line".to_string(),
                        ty: Type::Name("i32".to_string(), vec![]),
                    },
                    Field {
                        name: "stack".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
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
                name: "MemoryDump".to_string(),
                pub_: false,
                kind: TypeDefKind::Record(vec![
                    Field {
                        name: "fields".to_string(),
                        ty: Type::Name("string".to_string(), vec![]),
                    },
                    Field {
                        name: "count".to_string(),
                        ty: Type::Name("i32".to_string(), vec![]),
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
                self.emit_code(
                    crate::diagnostic::codes::E0409,
                    format!("type alias cycle detected: '{}' forms a cycle", name),
                );
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
        match item {
            Item::Func(f) => {
                self.set_pos(f.pos.0, f.pos.1);
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
                // Store where clause if present
                for wc in &f.where_clause {
                    self.where_clauses
                        .insert(f.name.clone(), (wc.type_param.clone(), wc.bounds.clone()));
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
                    name: actor.name.clone(),
                    pub_: actor.pub_,
                    kind: TypeDefKind::Record(
                        actor
                            .fields
                            .iter()
                            .map(|f| Field {
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
                    let mut params = vec![self_type];
                    params.extend(method.params.iter().map(|p| self.resolve_type(&p.ty)));
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    for (i, p) in method.params.iter().enumerate() {
                        self.check_type_well_formed(
                            &params[i + 1],
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
                if !self.declared_caps.insert(c.name.clone()) {
                    self.emit_code(
                        crate::diagnostic::codes::E0402,
                        format!("duplicate capability declaration '{}'", c.name),
                    );
                }
            }
            Item::Trait(trait_def) => {
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
                    let generic_names: Vec<String> =
                        method.generics.iter().map(|g| g.name.clone()).collect();
                    self.generic_scope.extend(generic_names.iter().cloned());
                    let mut params = vec![Type::Name(
                        impl_def.type_name.clone(),
                        impl_def.type_args.clone(),
                    )];
                    params.extend(method.params.iter().map(|p| self.resolve_type(&p.ty)));
                    let ret = method
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                    for (i, p) in method.params.iter().enumerate() {
                        self.check_type_well_formed(
                            &params[i + 1],
                            &format!("parameter '{}' of impl method '{}'", p.name, method.name),
                        );
                    }
                    self.check_type_well_formed(
                        &ret,
                        &format!("return type of impl method '{}'", method.name),
                    );
                    self.generic_scope
                        .truncate(self.generic_scope.len() - generic_names.len());
                    let key = format!("{}_{}", impl_def.type_name, method.name);
                    self.funcs.insert(key, (params, ret));
                }
                self.generic_scope
                    .truncate(self.generic_scope.len() - impl_generic_names.len());
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
                name, ty, value, ..
            } => {
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
                    if !self.types.contains_key(&type_name) {
                        let fields = state.payload.clone().unwrap_or_default();
                        let td = TypeDef {
                            name: type_name.clone(),
                            pub_: false,
                            kind: TypeDefKind::Record(fields),
                            generics: vec![],
                            derives: vec![],
                            attributes: vec![],
                        };
                        self.types.insert(type_name, td);
                        // Also register with unqualified name for use in transition bodies
                        if !self.types.contains_key(&state.name)
                            && !Self::is_builtin_type(&state.name)
                        {
                            let fields2 = state.payload.clone().unwrap_or_default();
                            let td2 = TypeDef {
                                name: state.name.clone(),
                                pub_: false,
                                kind: TypeDefKind::Record(fields2),
                                generics: vec![],
                                derives: vec![],
                                attributes: vec![],
                            };
                            self.types.insert(state.name.clone(), td2);
                        }
                    }
                }
                // Register transition functions.
                // Key includes from_state so overloads on different source
                // states coexist: `flow::Counter::inc::Zero`.
                // Signature: (from_state_payload, ...event_params) -> to_state
                // Multi-target transitions use the first target as the nominal
                // return type (call sites access common payload fields).
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
                    // Also keep a short key (last write wins) for name-only lookup
                    // when from_state cannot be inferred at the call site.
                    let short_key = format!("{}::{}", qualified, t.name);
                    self.funcs.insert(short_key, (params, ret));
                }
            }
            Item::Protocol(p) => {
                let qualified = format!("proto::{}", p.name);
                // Always register a protocol marker so empty-state protocols
                // (no payload fields) still resolve under `impl ProtocolName`.
                // Existence check uses `types` keys with prefix `proto::{name}`.
                if !self.types.contains_key(&qualified) {
                    let marker = TypeDef {
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
                    if !self.types.contains_key(&type_name) {
                        let fields = match &state.payload_type {
                            Some(payload_ty) => vec![Field {
                                name: "value".to_string(),
                                ty: payload_ty.clone(),
                            }],
                            None => vec![],
                        };
                        let td = TypeDef {
                            name: type_name.clone(),
                            pub_: false,
                            kind: TypeDefKind::Record(fields),
                            generics: vec![],
                            derives: vec![],
                            attributes: vec![],
                        };
                        self.types.insert(type_name, td);
                    }
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
                    let td = TypeDef {
                        name: "SessionChan".to_string(),
                        pub_: false,
                        kind: TypeDefKind::Record(vec![]),
                        generics: vec![GenericParam {
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
                    self.check_type_well_formed(
                        &field_ty,
                        &format!("actor field '{}'", field.name),
                    );
                    // Check field initialization if present
                    if let Some(init) = &field.init {
                        let init_ty = self.infer_expr(init, &mut vec![HashMap::new()]);
                        if !same_type(&field_ty, &init_ty) {
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
                            Span::single(self.current_line, self.current_col),
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
                }
                // Check impl method bodies with self bound to the implementing type
                let impl_generic_names: Vec<String> =
                    impl_def.generics.iter().map(|g| g.name.clone()).collect();
                self.generic_scope
                    .extend(impl_generic_names.iter().cloned());
                for method in &impl_def.methods {
                    self.set_pos(method.pos.0, method.pos.1);
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
                    self.cap_vars.push(HashMap::new());
                    self.check_block(&method.body, &ret, &mut scopes);
                    self.check_unconsumed_caps();
                    self.var_scopes.pop();
                    self.cap_vars.pop();
                    self.generic_scope
                        .truncate(self.generic_scope.len() - method_generic_names.len());
                }
                self.generic_scope
                    .truncate(self.generic_scope.len() - impl_generic_names.len());
            }
            Item::ExternBlock(_) => {
                // Extern blocks are collected but not type-checked in v1.1
            }
            Item::Const {
                name, ty, value, ..
            } => {
                let mut scopes: Vec<HashMap<String, Type>> = vec![HashMap::new()];
                let value_ty = self.infer_expr(value, &mut scopes);
                let const_ty = if let Some(declared_ty) = ty {
                    let resolved = self.resolve_type(declared_ty);
                    if !same_type(&resolved, &value_ty) {
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
                    resolved
                } else {
                    value_ty
                };
                // Register const type so that later items can reference it.
                // infer_item already does this; check_item must too.
                self.const_types.insert(name.clone(), const_ty);
            }
            Item::Flow(f) => {
                let qualified = format!("flow::{}", f.name);
                self.set_pos(
                    f.transitions.first().map(|t| t.pos).unwrap_or((1, 1)).0,
                    f.transitions.first().map(|t| t.pos).unwrap_or((1, 1)).1,
                );
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
                    // Type-check transition body with self in scope
                    if let Some(body) = &t.body {
                        let from_payload = f
                            .states
                            .iter()
                            .find(|s| s.name == t.from_state)
                            .and_then(|s| s.payload.as_ref());
                        let mut scopes: Vec<std::collections::HashMap<String, Type>> =
                            vec![std::collections::HashMap::new()];
                        // Add self with from-state's payload as a Record type
                        if from_payload.is_some() {
                            let type_name = format!("{}::{}", qualified, t.from_state);
                            let self_ty = Type::Name(type_name, vec![]);
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
                                &format!("transition '{}' param '{}' in flow '{}'", t.name, p.name, f.name),
                            );
                            scopes[0].insert(p.name.clone(), resolved);
                        }
                        let prev_ret = self.current_ret.take();
                        let prev_flow_targets = std::mem::take(&mut self.flow_return_targets);
                        let ret_type: Type =
                            if t.to_states.len() == 1 {
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
                        self.cap_vars.push(std::collections::HashMap::new());
                        // Type-check the body as a block
                        self.check_block(body, &ret_type, &mut scopes);
                        self.cap_vars.pop();
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
                        if let Some(ref proto_payload_ty) = ps.payload_type {
                            let flow_state = f.states.iter()
                                .find(|s| s.name == ps.name)
                                .unwrap();
                            let has_field = flow_state.payload.as_ref()
                                .map(|fields| fields.iter().any(|field| {
                                    let field_ty = self.resolve_type(&field.ty);
                                    let expected_ty = self.resolve_type(proto_payload_ty);
                                    self.unification.unify(&field_ty, &expected_ty).is_ok()
                                }))
                                .unwrap_or(false);
                            if !has_field {
                                self.emit_code(
                                    crate::diagnostic::codes::E0209,
                                    format!(
                                        "flow '{}' state '{}' must have a field matching protocol payload type {}",
                                        f.name, ps.name,
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
                    // 3. v0.29.36: Payload covariance / invariance rules.
                    //    - view-borrowed protocol fields: covariant (flow may have
                    //      wider payload type — extra fields allowed, width subtyping)
                    //    - mutate-borrowed protocol fields: invariant (flow must
                    //      exactly match protocol payload type)
                    //    The width subtyping (extra fields OK) is already enforced
                    //    in step 1. Here we add the invariant check for protocol
                    //    states with `mutate`-marked payload types.
                    //    Currently all protocol payload types are view-covariant
                    //    (the default), so this is a documentation point.
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
                                    let field_ty_name = match &field.ty {
                                        crate::ast::Type::Name(n, _) => n.clone(),
                                        _ => continue,
                                    };
                                    // Check if this field type is a subflow state
                                    // (i.e., another flow's state record name)
                                    let is_subflow_state = self.file.items.iter().any(|item| {
                                        if let crate::ast::Item::Flow(other_flow) = item {
                                            other_flow.states.iter().any(|s| s.name == field_ty_name)
                                                && other_flow.name != f.name
                                        } else {
                                            false
                                        }
                                    });
                                    if is_subflow_state {
                                        // Conservative projection: subflow state in
                                        // protocol payload → E0418 if protocol has
                                        // transitions that target this state
                                        let proto_targets_this = proto.transitions.iter()
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
                        let is_first = f.states.first().map(|first| first.name == s.name).unwrap_or(false);
                        if !is_first {
                            self.warnings.push(
                                crate::diagnostic::Diagnostic::warning_code(
                                    crate::diagnostic::codes::W0400,
                                    format!(
                                        "state '{}' in flow '{}' is unreachable (no transition targets to it)",
                                        s.name, f.name
                                    ),
                                    Span::single(self.current_line, self.current_col),
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
                                Span::single(self.current_line, self.current_col),
                            )
                        );
                    }
                }
            }
            Item::Protocol(p) => {
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
                        if let Type::Name(n, _) = &resolved {
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

    /// Well-formedness for a session type expression (v0.29.19).
    fn check_session_type_wf(&mut self, st: &crate::ast::SessionType, context: &str) {
        use crate::ast::SessionType;
        match st {
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
        }
    }
}
