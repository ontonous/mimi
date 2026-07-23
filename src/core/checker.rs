use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;
use std::collections::HashSet;

use super::unification::UnificationTable;

pub(crate) struct Checker<'a> {
    pub(crate) file: &'a File,
    pub(crate) errors: Vec<Diagnostic>,
    pub(crate) warnings: Vec<Diagnostic>,
    pub(crate) funcs: HashMap<String, (Vec<Type>, Type)>,
    pub(crate) aliases: HashMap<String, Type>,
    /// Declaration anchors for aliases. Alias-cycle validation runs after the
    /// whole (possibly multi-source) declaration graph is collected, so it
    /// must not rely on whichever item last updated `current_span`.
    pub(crate) alias_spans: HashMap<String, Span>,
    pub(crate) types: HashMap<String, TypeDef>,
    /// Track newtype definitions: name -> inner type (unresolved)
    pub(crate) newtypes: HashMap<String, Type>,
    /// Track trait definitions: trait_name -> list of method names
    pub(crate) traits: HashMap<String, Vec<String>>,
    /// Track trait generic params: trait_name -> list of generic param names
    pub(crate) trait_generics: HashMap<String, Vec<String>>,
    /// Track trait implementations: (trait_name, type_name) -> list of method names
    pub(crate) impls: HashMap<(String, String), Vec<String>>,
    /// Track where clauses for functions: func_name -> [(type_param, bounds), ...]
    /// CK-H6: store ALL type-param bounds (not a single overwritten entry).
    pub(crate) where_clauses: HashMap<String, Vec<(String, Vec<String>)>>,
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
    pub(crate) nested_func_params: HashMap<String, Vec<Param>>,
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
    /// Source-aware diagnostic context. Exact Expr/Stmt metadata temporarily
    /// replaces this value while that node is checked; declaration metadata
    /// remains the honest fallback for declaration-level checks.
    pub(crate) current_span: Span,
    /// C2: Unification table for type inference
    pub(crate) unification: UnificationTable,
    /// Top-level constant types: name -> type
    pub(crate) const_types: HashMap<String, Type>,
    /// Current function return type, used when type-checking block expressions
    /// so that `return` statements inside them are validated correctly.
    pub(crate) current_ret: Option<Type>,
    /// For multi-target flow transitions: list of target state types that a
    /// `return` statement is allowed to produce. When non-empty, `current_ret`
    /// is not used for return validation — each return is checked against all
    /// allowed types.
    pub(crate) flow_return_targets: Vec<Type>,
    /// FLOW-IDENTITY-001: root (first-declared) state names for each flow.
    /// Used to distinguish legitimate initial-state construction from state forgery.
    /// Qualified names: "FlowName::StateName".
    pub(crate) flow_root_states: std::collections::HashSet<String>,
    /// FLOW-IDENTITY-001 linear generation: variables consumed by flow transition
    /// calls. Maps variable name → transition description (e.g. "Counter::inc")
    /// for diagnostic messages. Cleared at each callable boundary.
    pub(crate) consumed_flow_vars: HashMap<String, String>,
    /// v0.31.12: session endpoints consumed by aliasing (`let b = a`).
    /// Using a consumed endpoint is E0426 (linear violation).
    pub(crate) consumed_session_vars: std::collections::HashSet<String>,
    /// FLOW-TURN-001: tracks whether we're inside a transition body and its
    /// declared `fails E` type. `None` = not in transition; `Some(None)` =
    /// transition without `fails`; `Some(Some(E))` = transition with `fails E`.
    pub(crate) transition_fails: Option<Option<Type>>,
    /// FLOW-TURN-001: maps transition key (e.g. "flow::Counter::inc::Zero")
    /// to the declared `fails E` error type. Used by `infer_method_call`
    /// to wrap the return type in `Result<Target, (Source, E)>`.
    pub(crate) transition_fails_types: HashMap<String, Type>,
    /// v0.29.49: variables bound to multi-target transition results.
    /// Maps variable name -> list of possible target state types.
    /// Direct field access on these variables is rejected (E0420) —
    /// the caller must use match to handle all possible states.
    pub(crate) multi_target_vars: HashMap<String, Vec<Type>>,
    /// Declared session types: name → body (v0.29.19).
    pub(crate) session_types: HashMap<String, crate::ast::SessionType>,
    /// Residual protocol for variables typed as `SessionChan<S>` within the
    /// current function body (order checking for session_send/recv/close).
    pub(crate) session_residuals: HashMap<String, crate::ast::SessionType>,
    /// Per-call session residual facts keyed by the clone-stable call node.
    pub(crate) session_actions: std::collections::BTreeMap<
        super::NodeId,
        std::collections::BTreeMap<super::resolved::ExpressionTypeKey, flow::CheckedSessionAction>,
    >,
    pub(crate) current_call_expression: Option<super::resolved::ExpressionTypeKey>,
    /// v0.29.23: names of `view`-borrowed params in the current function.
    pub(crate) view_params: std::collections::HashSet<String>,
    /// v0.29.23: names of `mutate`-borrowed params in the current function.
    pub(crate) mutate_params: std::collections::HashSet<String>,
    /// v0.29.27: nesting depth of `pinned { }` blocks (FFI anchor).
    pub(crate) in_pinned_depth: usize,
    /// Callable identity currently producing checker-finalized typed artifacts.
    pub(crate) current_callable_owner: Option<super::NodeId>,
    /// v0.31.2: Typed artifacts — schemes recorded during generalization.
    pub(crate) schemes: HashMap<super::NodeId, crate::core::phase::TypeScheme>,
    /// v0.31.2: Typed artifacts — resolved function signatures packed as ZonkedTy.
    pub(crate) zonked_func_types: HashMap<
        String,
        (
            Vec<crate::core::phase::ZonkedTy>,
            crate::core::phase::ZonkedTy,
        ),
    >,
    /// Nested callable signatures retain their stable owner instead of using
    /// the legacy global name directory, where local names can collide.
    pub(crate) zonked_nested_func_types: HashMap<
        super::NodeId,
        (
            Vec<crate::core::phase::ZonkedTy>,
            crate::core::phase::ZonkedTy,
        ),
    >,
    /// Per-callable expression types while the callable's unification table is live.
    pub(crate) current_expr_types: Option<(
        super::NodeId,
        HashMap<super::resolved::ExpressionTypeKey, Type>,
    )>,
    /// Final checker expression types, keyed temporarily by clone-stable source
    /// identity. The resolved walker replaces keys with stable NodeIds before
    /// ownership leaves `check_program`.
    pub(crate) zonked_expr_types: std::collections::BTreeMap<
        super::NodeId,
        std::collections::BTreeMap<
            super::resolved::ExpressionTypeKey,
            crate::core::phase::ZonkedTy,
        >,
    >,
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct DiagnosticDedupKey {
    code: Option<String>,
    message: String,
    severity: crate::diagnostic::Severity,
    source_id: crate::span::SourceId,
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
    notes: Vec<(String, crate::span::SourceId, usize, usize, usize, usize)>,
    help: Option<String>,
}

impl From<&Diagnostic> for DiagnosticDedupKey {
    fn from(diagnostic: &Diagnostic) -> Self {
        Self {
            code: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
            severity: diagnostic.severity,
            source_id: diagnostic.span.source_id,
            start_line: diagnostic.span.start_line,
            start_col: diagnostic.span.start_col,
            end_line: diagnostic.span.end_line,
            end_col: diagnostic.span.end_col,
            notes: diagnostic
                .notes
                .iter()
                .map(|note| {
                    (
                        note.message.clone(),
                        note.span.source_id,
                        note.span.start_line,
                        note.span.start_col,
                        note.span.end_line,
                        note.span.end_col,
                    )
                })
                .collect(),
            help: diagnostic.help.clone(),
        }
    }
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
            alias_spans: HashMap::new(),
            types: HashMap::new(),
            newtypes: HashMap::new(),
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
            nested_func_params: HashMap::new(),
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
            current_span: Span::UNKNOWN,
            unification: UnificationTable::new(),
            const_types: HashMap::new(),
            current_ret: None,
            flow_return_targets: Vec::new(),
            flow_root_states: std::collections::HashSet::new(),
            consumed_flow_vars: HashMap::new(),
            consumed_session_vars: std::collections::HashSet::new(),
            transition_fails: None,
            transition_fails_types: HashMap::new(),
            multi_target_vars: HashMap::new(),
            session_types: HashMap::new(),
            session_residuals: HashMap::new(),
            session_actions: std::collections::BTreeMap::new(),
            current_call_expression: None,
            view_params: std::collections::HashSet::new(),
            mutate_params: std::collections::HashSet::new(),
            in_pinned_depth: 0,
            current_callable_owner: None,
            schemes: HashMap::new(),
            zonked_func_types: HashMap::new(),
            zonked_nested_func_types: HashMap::new(),
            current_expr_types: None,
            zonked_expr_types: std::collections::BTreeMap::new(),
        }
    }

    pub(crate) fn begin_expression_type_capture(&mut self, owner: super::NodeId) {
        if self
            .current_expr_types
            .replace((owner, HashMap::new()))
            .is_some()
        {
            self.errors.push(Diagnostic::error(
                "TOOL-RESOLUTION-001: nested expression type capture is not supported",
                self.diagnostic_span(),
            ));
        }
    }

    pub(crate) fn finish_expression_type_capture(&mut self) {
        let Some((owner, expression_types)) = self.current_expr_types.take() else {
            return;
        };
        let mut zonked = std::collections::BTreeMap::new();
        for (key, ty) in expression_types {
            match crate::core::phase::ZonkedTy::from_expression_type(
                &ty,
                &mut self.unification,
            ) {
                Ok(ty) => {
                    zonked.insert(key, ty);
                }
                Err(error) => self.errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: expression {:?} in '{}' did not finalize to a monotype: {}",
                        key, owner.0, error
                    ),
                    self.diagnostic_span(),
                )),
            }
        }
        self.zonked_expr_types.insert(owner, zonked);
    }

    pub(crate) fn record_nested_function_signature(
        &mut self,
        owner: super::NodeId,
        parameters: &[Type],
        result: &Type,
    ) {
        let finalized = (|| {
            let parameters = parameters
                .iter()
                .map(|parameter| {
                    self.unification
                        .zonk(parameter)
                        .and_then(crate::core::phase::ZonkedTy::from_resolved)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let result = self
                .unification
                .zonk(result)
                .and_then(crate::core::phase::ZonkedTy::from_resolved)?;
            Ok::<_, crate::core::unification::ResolveError>((parameters, result))
        })();
        match finalized {
            Ok(signature) => {
                self.zonked_nested_func_types.insert(owner, signature);
            }
            Err(error) => self.errors.push(Diagnostic::error(
                format!("TOOL-RESOLUTION-001: nested callable signature is not zonked: {error}"),
                self.diagnostic_span(),
            )),
        }
    }

    pub(crate) fn record_expression_type(&mut self, expr: &Expr, ty: &Type) {
        let Some((_, expression_types)) = &mut self.current_expr_types else {
            return;
        };
        expression_types.insert(super::resolved::expression_type_key(expr), ty.clone());
        if let Expr::Call(callee, _) = expr.unlocated() {
            // The surface callee expression is normalized into a closed
            // ResolvedCallee identity and is not a value node in ResolvedBody.
            expression_types.remove(&super::resolved::expression_type_key(callee));
        }
    }

    pub(crate) fn begin_callable(&mut self, owner: super::NodeId) -> Option<super::NodeId> {
        self.consumed_flow_vars.clear();
        self.consumed_session_vars.clear();
        self.current_callable_owner.replace(owner)
    }

    pub(crate) fn end_callable(&mut self, previous: Option<super::NodeId>) {
        self.current_callable_owner = previous;
    }

    /// Set the current position for fallback error spans.
    pub(crate) fn set_pos(&mut self, line: usize, col: usize) {
        self.current_line = line;
        self.current_col = col;
        self.current_span = Span::single(line, col).with_source(self.current_span.source_id);
    }

    /// Replace the current source-aware diagnostic context.
    pub(crate) fn set_span(&mut self, span: Span) {
        self.current_span = span;
        self.current_line = span.start_line;
        self.current_col = span.start_col;
    }

    /// Return an honest diagnostic span. Unknown context stays UNKNOWN rather
    /// than becoming a fabricated `(0,0)` point that tooling might publish on
    /// the active document.
    pub(crate) fn diagnostic_span(&self) -> Span {
        if self.current_span.start_line == 0 || self.current_span.start_col == 0 {
            Span::UNKNOWN
        } else {
            self.current_span
        }
    }

    pub(crate) fn replace_span(&mut self, span: Option<Span>) -> Span {
        let previous = self.current_span;
        if let Some(span) = span {
            self.set_span(span);
        }
        previous
    }

    pub(crate) fn check(&mut self) -> Result<(), Vec<Diagnostic>> {
        self.collect_decls();
        self.emit_progressive_migration_hint();
        for item in &self.file.items {
            self.check_item(item);
        }
        if self.errors.is_empty() {
            Ok(())
        } else {
            // P1-7: deduplicate truly identical diagnostics. Source identity,
            // range, notes and help are semantic: collapsing two dependency
            // diagnostics with the same prose would lose cross-file errors.
            // which can occur when a method-call expression inside a
            // multi-arg expression is type-checked along multiple paths.
            let mut seen: std::collections::HashSet<DiagnosticDedupKey> =
                std::collections::HashSet::new();
            let mut deduped: Vec<Diagnostic> = Vec::with_capacity(self.errors.len());
            for e in std::mem::take(&mut self.errors) {
                let key = DiagnosticDedupKey::from(&e);
                if seen.insert(key) {
                    deduped.push(e);
                }
            }
            Err(deduped)
        }
    }

    pub(crate) fn emit_code(&mut self, code: &str, msg: impl Into<String>) {
        let span = self.diagnostic_span();
        self.errors.push(Diagnostic::error_code(code, msg, span));
    }

    pub(crate) fn emit_warning_code(&mut self, code: &str, msg: impl Into<String>) {
        let span = self.diagnostic_span();
        self.warnings
            .push(Diagnostic::warning_code(code, msg, span));
    }

    /// v0.29.23: true when any view/mutate param is active in this function.
    pub(crate) fn lexical_borrow_active(&self) -> bool {
        !self.view_params.is_empty() || !self.mutate_params.is_empty()
    }

    /// v0.29.23: reject flow transitions while a lexical view/mutate borrow is live.
    pub(crate) fn reject_transition_under_borrow(&mut self, what: &str) {
        if self.lexical_borrow_active() {
            let names: Vec<_> = self
                .view_params
                .iter()
                .chain(self.mutate_params.iter())
                .cloned()
                .collect();
            self.emit_code(
                crate::diagnostic::codes::E0415,
                format!(
                    "cannot {} while view/mutate borrow of [{}] is active (lexical borrow ends at function return)",
                    what,
                    names.join(", ")
                ),
            );
        }
        // v0.29.27: FFI pinned region freezes transitions (Active→FFI_Pinned semantics).
        if self.in_pinned_depth > 0 {
            self.emit_code(
                crate::diagnostic::codes::E0416,
                format!(
                    "cannot {} while inside `pinned {{ }}` (FFI anchor: no state transfer until unpin)",
                    what
                ),
            );
        }
    }
    pub(crate) fn emit_progressive_migration_hint(&mut self) {
        if self.file.implicit_single {
            return; // still in script mode — no migration needed
        }
        let Some(user_flow_span) = self.file.items.iter().find_map(|item| match item {
            crate::ast::Item::Flow(flow) if flow.meta.origin == AstOrigin::User => {
                Some(flow.meta.span)
            }
            _ => None,
        }) else {
            return;
        };
        if !crate::progressive::has_top_level_main(self.file) {
            return;
        }
        let locals = crate::progressive::main_local_names(self.file);
        let local_hint = if locals.is_empty() {
            String::new()
        } else {
            let shown: Vec<_> = locals.into_iter().take(5).collect();
            format!(
                " Local variable(s) in main ({}) previously belonged to the implicit Single payload — declare them in your first Flow state if they must persist across transitions.",
                shown.join(", ")
            )
        };
        self.warnings.push(Diagnostic::warning_code(
            crate::diagnostic::codes::W011,
            format!(
                "detected explicit `flow` — progressive script mode (implicit Single) is disabled.{}",
                local_hint
            ),
            user_flow_span,
        ));
    }

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
    /// v0.31.2: Uses `CollectVarsFolder` + `RemapFolder` from type_folder infrastructure.
    /// Also records a `TypeScheme` in `self.schemes` for checker artifacts.
    pub(crate) fn generalize(&mut self, ty: &Type, env: &HashMap<String, Type>) -> Type {
        let resolved = self.unification.resolve(ty);
        if matches!(resolved.unlocated(), Type::ForAll(..)) {
            return resolved;
        }
        // Collect free TypeVars using CollectVarsFolder.
        let mut collector = crate::core::type_folder::CollectVarsFolder::new();
        crate::core::type_folder::walk_type(resolved.clone(), &mut collector);
        collector.vars.sort();
        collector.vars.dedup();
        let env_vars = self.collect_env_type_vars(env);
        let generalized: Vec<u32> = collector
            .vars
            .into_iter()
            .filter(|v| !env_vars.contains(v))
            .collect();
        if generalized.is_empty() {
            resolved
        } else {
            // Remap original TypeVar IDs to sequential indices 0,1,2,...
            let mut remap: HashMap<u32, u32> = HashMap::new();
            for (i, old_id) in generalized.iter().enumerate() {
                remap.insert(*old_id, i as u32);
            }
            let mut remapper = crate::core::type_folder::RemapFolder::new(remap);
            let remapped_body = crate::core::type_folder::walk_type(resolved, &mut remapper);
            let param_names: Vec<String> =
                (0..generalized.len()).map(|i| format!("T{}", i)).collect();
            let forall = Type::ForAll(param_names, Box::new(remapped_body.clone()));
            // Record the monotype body separately from its binders. The legacy
            // `Type::ForAll` wrapper remains only in the inference environment.
            if let Some(owner) = &self.current_callable_owner {
                let binders: Vec<_> = (0..generalized.len() as u32)
                    .map(crate::core::phase::InferVarId)
                    .collect();
                if let Ok(scheme) = crate::core::phase::TypeScheme::new(binders, remapped_body) {
                    self.schemes.insert(owner.clone(), scheme);
                }
            }
            forall
        }
    }

    /// C4: Instantiate a ForAll type — replace bound variables with fresh TypeVars.
    ///
    /// When using a polymorphic function, call this to get a fresh copy.
    ///
    /// v0.31.2: Uses `RemapFolder` from type_folder infrastructure.
    pub(crate) fn instantiate(&mut self, ty: &Type) -> Type {
        match ty.unlocated() {
            Type::ForAll(params, body) => {
                let mut substitutions = HashMap::new();
                for (i, _param) in params.iter().enumerate() {
                    let fresh = self.fresh_var();
                    if let Type::TypeVar(id) = fresh {
                        substitutions.insert(i as u32, id);
                    }
                }
                let mut remapper = crate::core::type_folder::RemapFolder::new(substitutions);
                let substituted =
                    crate::core::type_folder::walk_type((**body).clone(), &mut remapper);
                // CK-C3: peel nested ForAll after substitution so polymorphic
                // let bindings do not leave residual quantifiers in the type.
                self.instantiate(&substituted)
            }
            _ => ty.clone(),
        }
    }

    /// Collect TypeVar IDs that appear in the environment.
    fn collect_env_type_vars(&self, env: &HashMap<String, Type>) -> Vec<u32> {
        let mut vars = Vec::new();
        for ty in env.values() {
            let mut collector = crate::core::type_folder::CollectVarsFolder::new();
            crate::core::type_folder::walk_type(ty.clone(), &mut collector);
            vars.extend(collector.vars);
        }
        vars.sort();
        vars.dedup();
        vars
    }

    /// v0.31.2: Zonk all resolved function types and store in zonked_func_types.
    /// Called before extracting artifacts from the checker.
    pub(crate) fn finalize_zonked_func_types(&mut self) {
        fn function_span(items: &[Item], prefix: &str, target: &str) -> Option<Span> {
            for item in items {
                match item {
                    Item::Func(function) => {
                        let qualified = if prefix.is_empty() {
                            function.name.clone()
                        } else {
                            format!("{prefix}::{}", function.name)
                        };
                        if qualified == target {
                            return Some(function.meta.span);
                        }
                    }
                    Item::Module(module) => {
                        let nested = if prefix.is_empty() {
                            module.name.clone()
                        } else {
                            format!("{prefix}::{}", module.name)
                        };
                        if let Some(span) = function_span(&module.items, &nested, target) {
                            return Some(span);
                        }
                    }
                    _ => {}
                }
            }
            None
        }

        let mut zonked = HashMap::new();
        for (name, (params, ret)) in self.funcs.clone() {
            let finalized = (|| {
                let params = params
                    .iter()
                    .map(|param| {
                        self.unification
                            .zonk(param)
                            .and_then(crate::core::phase::ZonkedTy::from_resolved)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let ret = self
                    .unification
                    .zonk(&ret)
                    .and_then(crate::core::phase::ZonkedTy::from_resolved)?;
                Ok::<_, crate::core::unification::ResolveError>((params, ret))
            })();
            match finalized {
                Ok(signature) => {
                    zonked.insert(name, signature);
                }
                Err(error) => {
                    let span = function_span(&self.file.items, "", &name)
                        .unwrap_or_else(|| self.diagnostic_span());
                    self.errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0200,
                        format!(
                            "TOOL-RESOLUTION-001: function '{}' did not finalize to a monotype: {}",
                            name, error
                        ),
                        span,
                    ));
                }
            }
        }
        self.zonked_func_types = zonked;
    }
}

#[cfg(test)]
mod diagnostic_dedup_tests {
    use super::DiagnosticDedupKey;
    use crate::diagnostic::Diagnostic;
    use crate::span::{SourceId, Span};

    #[test]
    fn identical_messages_in_distinct_sources_are_not_deduplicated() {
        let first = Diagnostic::error(
            "same error",
            Span::single(3, 4).with_source(SourceId::new(1)),
        );
        let second = Diagnostic::error(
            "same error",
            Span::single(3, 4).with_source(SourceId::new(2)),
        );
        assert_ne!(
            DiagnosticDedupKey::from(&first),
            DiagnosticDedupKey::from(&second)
        );
    }
}

// Raw-AST use collection is retained only for the test-only CFG oracle.
#[cfg(test)]
mod borrow;
pub(crate) mod flow;
mod func;
mod generics;
mod items;
mod pattern;
mod types;
mod vars;
