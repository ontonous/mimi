use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;
use std::collections::HashSet;

use super::borrow::BorrowState;
use super::unification::UnificationTable;

pub(crate) struct Checker<'a> {
    pub(crate) file: &'a File,
    pub(crate) errors: Vec<Diagnostic>,
    pub(crate) warnings: Vec<Diagnostic>,
    pub(crate) funcs: HashMap<String, (Vec<Type>, Type)>,
    pub(crate) aliases: HashMap<String, Type>,
    pub(crate) types: HashMap<String, TypeDef>,
    /// Track newtype definitions: name -> inner type (unresolved)
    pub(crate) newtypes: HashMap<String, Type>,
    /// Track linear capabilities in scope: name -> consumed
    pub(crate) cap_vars: Vec<HashMap<String, bool>>,
    /// Track borrow state of variables: name -> borrow state
    pub(crate) borrows: Vec<HashMap<String, BorrowState>>,
    /// Track field-level borrow state: (var_name, field_path) -> borrow state
    pub(crate) field_borrows: Vec<HashMap<(String, Vec<String>), BorrowState>>,
    /// Track trait definitions: trait_name -> list of method names
    pub(crate) traits: HashMap<String, Vec<String>>,
    /// Track trait generic params: trait_name -> list of generic param names
    pub(crate) trait_generics: HashMap<String, Vec<String>>,
    /// Track trait implementations: (trait_name, type_name) -> list of method names
    pub(crate) impls: HashMap<(String, String), Vec<String>>,
    /// Track where clauses for functions: func_name -> (type_param, bounds)
    pub(crate) where_clauses: HashMap<String, (String, Vec<String>)>,
    /// Track effects for functions: func_name -> list of effect names
    pub(crate) func_effects: HashMap<String, Vec<String>>,
    /// Track available effects in current scope
    pub(crate) available_effects: Vec<HashMap<String, bool>>,
    /// Track declared capability names for cross-validation of `with` clauses
    pub(crate) declared_caps: HashSet<String>,
    /// Strict mode: enforce $$ lock semantics
    pub(crate) strict: bool,
    /// Track variable scopes for shadowing detection
    pub(crate) var_scopes: Vec<HashMap<String, usize>>,
    /// Track mutable variables: name -> is_mut
    pub(crate) mut_vars: Vec<HashMap<String, bool>>,
    /// Track generic parameters per function: func_name -> generic params
    pub(crate) func_generics: HashMap<String, Vec<GenericParam>>,
    /// Track generic parameters per type def: type_name -> generic params
    pub(crate) type_generics: HashMap<String, Vec<GenericParam>>,
    /// Track methods available on types via traits: type_name -> list of (trait_name, method_name)
    pub(crate) type_methods: HashMap<String, Vec<(String, String)>>,
    /// Track trait method signatures: (trait_name, method_name) -> (param_types, return_type)
    pub(crate) trait_method_sigs: HashMap<(String, String), (Vec<Type>, Type)>,
    /// Track imported module names (from `use` statements)
    pub(crate) use_imports: Vec<String>,
    /// Track current module path for qualified names
    pub(crate) module_path: Vec<String>,
    /// Track loop nesting depth for break/continue validation
    pub(crate) loop_depth: usize,
    /// Track generic parameters in scope while checking signatures
    pub(crate) generic_scope: Vec<String>,
    /// Track arena block nesting depth for escape detection
    pub(crate) arena_depth: usize,
    /// Current item/function line-col for fallback error positioning
    pub(crate) current_line: usize,
    pub(crate) current_col: usize,
    /// C2: Unification table for type inference
    pub(crate) unification: UnificationTable,
    /// Top-level constant types: name -> type
    pub(crate) const_types: HashMap<String, Type>,
    /// Current function return type, used when type-checking block expressions
    /// so that `return` statements inside them are validated correctly.
    pub(crate) current_ret: Option<Type>,
}

#[allow(dead_code)]
impl<'a> Checker<'a> {
    pub(crate) fn new(file: &'a File) -> Self {
        Self {
            file,
            errors: Vec::new(),
            warnings: Vec::new(),
            funcs: HashMap::new(),
            aliases: HashMap::new(),
            types: HashMap::new(),
            newtypes: HashMap::new(),
            cap_vars: vec![HashMap::new()],
            borrows: vec![HashMap::new()],
            field_borrows: vec![HashMap::new()],
            traits: HashMap::new(),
            trait_generics: HashMap::new(),
            impls: HashMap::new(),
            where_clauses: HashMap::new(),
            func_effects: HashMap::new(),
            available_effects: vec![HashMap::new()],
            declared_caps: HashSet::new(),
            strict: false,
            var_scopes: vec![HashMap::new()],
            mut_vars: vec![HashMap::new()],
            func_generics: HashMap::new(),
            type_generics: HashMap::new(),
            type_methods: HashMap::new(),
            trait_method_sigs: HashMap::new(),
            use_imports: Vec::new(),
            module_path: Vec::new(),
            loop_depth: 0,
            generic_scope: Vec::new(),
            arena_depth: 0,
            current_line: 0,
            current_col: 0,
            unification: UnificationTable::new(),
            const_types: HashMap::new(),
            current_ret: None,
        }
    }

    /// Set the current position for fallback error spans.
    pub(crate) fn set_pos(&mut self, line: usize, col: usize) {
        self.current_line = line;
        self.current_col = col;
    }

    pub(crate) fn check(&mut self) -> Result<(), Vec<Diagnostic>> {
        self.collect_decls();
        for item in &self.file.items {
            self.check_item(item);
        }
        if self.errors.is_empty() {
            Ok(())
        } else {
            // P1-7: deduplicate identical errors (same code + message),
            // which can occur when a method-call expression inside a
            // multi-arg expression is type-checked along multiple paths.
            let mut seen: std::collections::HashSet<(Option<String>, String)> =
                std::collections::HashSet::new();
            let mut deduped: Vec<Diagnostic> = Vec::with_capacity(self.errors.len());
            for e in std::mem::take(&mut self.errors) {
                let key = (e.code.clone(), e.message.clone());
                if seen.insert(key) {
                    deduped.push(e);
                }
            }
            Err(deduped)
        }
    }

    pub(crate) fn emit_code(&mut self, code: &str, msg: impl Into<String>) {
        let span = Span::single(self.current_line, self.current_col);
        self.errors.push(Diagnostic::error_code(code, msg, span));
    }

    pub(crate) fn emit_warning_code(&mut self, code: &str, msg: impl Into<String>) {
        let span = Span::single(self.current_line, self.current_col);
        self.warnings
            .push(Diagnostic::warning_code(code, msg, span));
    }

    /// C2: Allocate a fresh type variable for inference.
    pub(crate) fn fresh_var(&mut self) -> Type {
        let id = self.unification.fresh_var();
        Type::TypeVar(id)
    }

    /// C2: Unify two types, emitting a diagnostic on failure.
    pub(crate) fn unify_types(&mut self, expected: &Type, actual: &Type) -> bool {
        match self.unification.unify(expected, actual) {
            Ok(()) => true,
            Err(e) => {
                self.emit_code(
                    crate::diagnostic::codes::E0209,
                    format!(
                        "type mismatch: expected {}, found {} ({})",
                        crate::core::helpers::fmt_type(expected),
                        crate::core::helpers::fmt_type(actual),
                        e
                    ),
                );
                false
            }
        }
    }

    /// C4: Generalize a type — wrap free TypeVars not in the environment in ForAll.
    ///
    /// After solving a let binding, call this to make the type polymorphic.
    /// Free TypeVars (not bound in the environment) become universally quantified.
    ///
    /// Bug 6 fix: single-traversal resolve-and-collect (previously resolve + collect
    /// free vars were two separate O(N·D) tree walks; now done in one pass).
    /// Bug 10 fix: remap free TypeVar IDs to sequential indices 0,1,2... in the
    /// ForAll body so that `instantiate` (which substitutes TypeVar(i)→fresh) works correctly.
    pub(crate) fn generalize(&mut self, ty: &Type, env: &HashMap<String, Type>) -> Type {
        let (resolved, free_vars) = self.resolve_and_collect_free_vars(ty);
        let env_vars = self.collect_env_type_vars(env);
        let generalized: Vec<u32> = free_vars
            .into_iter()
            .filter(|v| !env_vars.contains(v))
            .collect();
        if generalized.is_empty() {
            resolved
        } else {
            // Bug 10 fix: remap original TypeVar IDs to sequential indices 0,1,2,...
            // so that instantiate() can correctly substitute TypeVar(i) → fresh_var.
            let mut remap: HashMap<u32, u32> = HashMap::new();
            for (i, old_id) in generalized.iter().enumerate() {
                remap.insert(*old_id, i as u32);
            }
            let remapped_body = self.remap_type_vars(&resolved, &remap);
            let param_names: Vec<String> =
                (0..generalized.len()).map(|i| format!("T{}", i)).collect();
            Type::ForAll(param_names, Box::new(remapped_body))
        }
    }

    /// Remap TypeVar IDs in a type according to the given mapping (Bug 10 fix).
    fn remap_type_vars(&self, ty: &Type, remap: &HashMap<u32, u32>) -> Type {
        match ty {
            Type::TypeVar(id) => {
                if let Some(&new_id) = remap.get(id) {
                    Type::TypeVar(new_id)
                } else {
                    ty.clone()
                }
            }
            Type::Option(inner) => Type::Option(Box::new(self.remap_type_vars(inner, remap))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.remap_type_vars(ok, remap)),
                Box::new(self.remap_type_vars(err, remap)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| self.remap_type_vars(e, remap))
                    .collect(),
            ),
            Type::Func(args, ret) | Type::ExternFunc(args, ret) => Type::Func(
                args.iter()
                    .map(|a| self.remap_type_vars(a, remap))
                    .collect(),
                Box::new(self.remap_type_vars(ret, remap)),
            ),
            Type::Ref(lt, inner) => {
                Type::Ref(lt.clone(), Box::new(self.remap_type_vars(inner, remap)))
            }
            Type::RefMut(lt, inner) => {
                Type::RefMut(lt.clone(), Box::new(self.remap_type_vars(inner, remap)))
            }
            Type::Shared(inner) => Type::Shared(Box::new(self.remap_type_vars(inner, remap))),
            Type::LocalShared(inner) => {
                Type::LocalShared(Box::new(self.remap_type_vars(inner, remap)))
            }
            Type::Weak(inner) => Type::Weak(Box::new(self.remap_type_vars(inner, remap))),
            Type::WeakLocal(inner) => Type::WeakLocal(Box::new(self.remap_type_vars(inner, remap))),
            Type::RawPtr(inner) => Type::RawPtr(Box::new(self.remap_type_vars(inner, remap))),
            Type::RawPtrMut(inner) => Type::RawPtrMut(Box::new(self.remap_type_vars(inner, remap))),
            Type::CShared(inner) => Type::CShared(Box::new(self.remap_type_vars(inner, remap))),
            Type::CBorrow(inner) => Type::CBorrow(Box::new(self.remap_type_vars(inner, remap))),
            Type::CBorrowMut(inner) => {
                Type::CBorrowMut(Box::new(self.remap_type_vars(inner, remap)))
            }
            Type::CBuffer(inner) => Type::CBuffer(Box::new(self.remap_type_vars(inner, remap))),
            Type::Array(inner, size) => {
                Type::Array(Box::new(self.remap_type_vars(inner, remap)), *size)
            }
            Type::Slice(inner) => Type::Slice(Box::new(self.remap_type_vars(inner, remap))),
            Type::Newtype(name, inner) => {
                Type::Newtype(name.clone(), Box::new(self.remap_type_vars(inner, remap)))
            }
            Type::Name(name, args) => Type::Name(
                name.clone(),
                args.iter()
                    .map(|a| self.remap_type_vars(a, remap))
                    .collect(),
            ),
            Type::ForAll(params, body) => {
                Type::ForAll(params.clone(), Box::new(self.remap_type_vars(body, remap)))
            }
            _ => ty.clone(),
        }
    }

    /// Resolve a type and collect free TypeVars in a single traversal (Bug 6 fix).
    fn resolve_and_collect_free_vars(&mut self, ty: &Type) -> (Type, Vec<u32>) {
        let mut free_vars = Vec::new();
        let resolved = self.resolve_and_collect_inner(ty, &mut free_vars);
        free_vars.sort();
        free_vars.dedup();
        (resolved, free_vars)
    }

    /// Combined resolve + collect free TypeVars inner loop.
    fn resolve_and_collect_inner(&mut self, ty: &Type, free_vars: &mut Vec<u32>) -> Type {
        match ty {
            Type::TypeVar(id) => {
                let root = self.unification.find(*id);
                if let Some(bound) = self.unification.get_binding(root).cloned() {
                    let resolved = self.resolve_and_collect_inner(&bound, free_vars);
                    free_vars.push(root);
                    resolved
                } else {
                    free_vars.push(root);
                    Type::TypeVar(root)
                }
            }
            Type::Option(inner) => {
                Type::Option(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::Result(ok, err) => Type::Result(
                Box::new(self.resolve_and_collect_inner(ok, free_vars)),
                Box::new(self.resolve_and_collect_inner(err, free_vars)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| self.resolve_and_collect_inner(e, free_vars))
                    .collect(),
            ),
            Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
                let resolved_args = args
                    .iter()
                    .map(|a| self.resolve_and_collect_inner(a, free_vars))
                    .collect();
                Type::Func(
                    resolved_args,
                    Box::new(self.resolve_and_collect_inner(ret, free_vars)),
                )
            }
            Type::Ref(lt, inner) => Type::Ref(
                lt.clone(),
                Box::new(self.resolve_and_collect_inner(inner, free_vars)),
            ),
            Type::RefMut(lt, inner) => Type::RefMut(
                lt.clone(),
                Box::new(self.resolve_and_collect_inner(inner, free_vars)),
            ),
            Type::Shared(inner) => {
                Type::Shared(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::LocalShared(inner) => {
                Type::LocalShared(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::Weak(inner) => {
                Type::Weak(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::WeakLocal(inner) => {
                Type::WeakLocal(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::RawPtr(inner) => {
                Type::RawPtr(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::RawPtrMut(inner) => {
                Type::RawPtrMut(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::CShared(inner) => {
                Type::CShared(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::CBorrow(inner) => {
                Type::CBorrow(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::CBorrowMut(inner) => {
                Type::CBorrowMut(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::CBuffer(inner) => {
                Type::CBuffer(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::Array(inner, size) => Type::Array(
                Box::new(self.resolve_and_collect_inner(inner, free_vars)),
                *size,
            ),
            Type::Slice(inner) => {
                Type::Slice(Box::new(self.resolve_and_collect_inner(inner, free_vars)))
            }
            Type::Newtype(name, inner) => Type::Newtype(
                name.clone(),
                Box::new(self.resolve_and_collect_inner(inner, free_vars)),
            ),
            Type::Name(name, args) => Type::Name(
                name.clone(),
                args.iter()
                    .map(|a| self.resolve_and_collect_inner(a, free_vars))
                    .collect(),
            ),
            Type::ForAll(params, body) => Type::ForAll(
                params.clone(),
                Box::new(self.resolve_and_collect_inner(body, free_vars)),
            ),
            _ => ty.clone(),
        }
    }

    /// C4: Instantiate a ForAll type — replace bound variables with fresh TypeVars.
    ///
    /// When using a polymorphic function, call this to get a fresh copy.
    ///
    /// Bug-8 clarification: params (Vec<String>) are labels for error messages only,
    /// not used for type substitution. The actual substitution uses integer indices
    /// (i as u32) matching TypeVar IDs in the body. This avoids confusion between
    /// user-defined type parameters (Type::Name) and inference variables (TypeVar).
    pub(crate) fn instantiate(&mut self, ty: &Type) -> Type {
        match ty {
            Type::ForAll(params, body) => {
                let mut substitutions = HashMap::new();
                for (i, _param) in params.iter().enumerate() {
                    let fresh = self.fresh_var();
                    // Map the bound variable name to a fresh TypeVar
                    // The body uses TypeVar(i) for the i-th bound variable
                    if let Type::TypeVar(id) = fresh {
                        substitutions.insert(i as u32, id);
                    }
                }
                self.substitute_type_vars(body, &substitutions)
            }
            _ => ty.clone(),
        }
    }

    fn collect_type_vars_inner(&self, ty: &Type, vars: &mut Vec<u32>) {
        match ty {
            Type::TypeVar(id) => vars.push(*id),
            Type::ForAll(_, body) => self.collect_type_vars_inner(body, vars),
            Type::Option(inner) => self.collect_type_vars_inner(inner, vars),
            Type::Result(ok, err) => {
                self.collect_type_vars_inner(ok, vars);
                self.collect_type_vars_inner(err, vars);
            }
            Type::Tuple(elems) => {
                for e in elems {
                    self.collect_type_vars_inner(e, vars);
                }
            }
            Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
                for a in args {
                    self.collect_type_vars_inner(a, vars);
                }
                self.collect_type_vars_inner(ret, vars);
            }
            Type::Ref(_, inner)
            | Type::RefMut(_, inner)
            | Type::Shared(inner)
            | Type::LocalShared(inner)
            | Type::Weak(inner)
            | Type::WeakLocal(inner)
            | Type::RawPtr(inner)
            | Type::RawPtrMut(inner)
            | Type::CShared(inner)
            | Type::CBorrow(inner)
            | Type::CBorrowMut(inner)
            | Type::CBuffer(inner)
            | Type::Array(inner, _)
            | Type::Slice(inner)
            | Type::Newtype(_, inner) => self.collect_type_vars_inner(inner, vars),
            Type::Name(_, args) => {
                for a in args {
                    self.collect_type_vars_inner(a, vars);
                }
            }
            _ => {}
        }
    }

    /// Collect TypeVar IDs that appear in the environment.
    fn collect_env_type_vars(&self, env: &HashMap<String, Type>) -> Vec<u32> {
        let mut vars = Vec::new();
        for ty in env.values() {
            self.collect_type_vars_inner(ty, &mut vars);
        }
        vars.sort();
        vars.dedup();
        vars
    }

    /// Substitute TypeVar IDs in a type with new IDs.
    fn substitute_type_vars(&self, ty: &Type, subs: &HashMap<u32, u32>) -> Type {
        match ty {
            Type::TypeVar(id) => {
                if let Some(new_id) = subs.get(id) {
                    Type::TypeVar(*new_id)
                } else {
                    ty.clone()
                }
            }
            Type::Option(inner) => Type::Option(Box::new(self.substitute_type_vars(inner, subs))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.substitute_type_vars(ok, subs)),
                Box::new(self.substitute_type_vars(err, subs)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| self.substitute_type_vars(e, subs))
                    .collect(),
            ),
            Type::Func(args, ret) => Type::Func(
                args.iter()
                    .map(|a| self.substitute_type_vars(a, subs))
                    .collect(),
                Box::new(self.substitute_type_vars(ret, subs)),
            ),
            Type::Ref(lt, inner) => {
                Type::Ref(lt.clone(), Box::new(self.substitute_type_vars(inner, subs)))
            }
            Type::RefMut(lt, inner) => {
                Type::RefMut(lt.clone(), Box::new(self.substitute_type_vars(inner, subs)))
            }
            Type::Name(name, args) => Type::Name(
                name.clone(),
                args.iter()
                    .map(|a| self.substitute_type_vars(a, subs))
                    .collect(),
            ),
            // Bug 7 fix: added missing container variants
            Type::Array(inner, size) => {
                Type::Array(Box::new(self.substitute_type_vars(inner, subs)), *size)
            }
            Type::Slice(inner) => Type::Slice(Box::new(self.substitute_type_vars(inner, subs))),
            Type::Shared(inner) => Type::Shared(Box::new(self.substitute_type_vars(inner, subs))),
            Type::LocalShared(inner) => {
                Type::LocalShared(Box::new(self.substitute_type_vars(inner, subs)))
            }
            Type::Weak(inner) => Type::Weak(Box::new(self.substitute_type_vars(inner, subs))),
            Type::WeakLocal(inner) => {
                Type::WeakLocal(Box::new(self.substitute_type_vars(inner, subs)))
            }
            Type::RawPtr(inner) => Type::RawPtr(Box::new(self.substitute_type_vars(inner, subs))),
            Type::RawPtrMut(inner) => {
                Type::RawPtrMut(Box::new(self.substitute_type_vars(inner, subs)))
            }
            Type::CShared(inner) => Type::CShared(Box::new(self.substitute_type_vars(inner, subs))),
            Type::CBorrow(inner) => Type::CBorrow(Box::new(self.substitute_type_vars(inner, subs))),
            Type::CBorrowMut(inner) => {
                Type::CBorrowMut(Box::new(self.substitute_type_vars(inner, subs)))
            }
            Type::CBuffer(inner) => Type::CBuffer(Box::new(self.substitute_type_vars(inner, subs))),
            Type::Newtype(name, inner) => Type::Newtype(
                name.clone(),
                Box::new(self.substitute_type_vars(inner, subs)),
            ),
            Type::ExternFunc(args, ret) => Type::ExternFunc(
                args.iter()
                    .map(|a| self.substitute_type_vars(a, subs))
                    .collect(),
                Box::new(self.substitute_type_vars(ret, subs)),
            ),
            Type::ForAll(params, body) => Type::ForAll(
                params.clone(),
                Box::new(self.substitute_type_vars(body, subs)),
            ),
            _ => ty.clone(),
        }
    }
}

mod borrow;
mod func;
mod generics;
mod items;
mod pattern;
mod types;
mod vars;
