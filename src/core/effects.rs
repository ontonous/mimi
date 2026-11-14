//! Checker owned effect vocabulary and conservative interprocedural inference.
//!
//! Effects are deliberately a small, closed set.  This module is independent
//! of the parser spelling: the surface `FuncDef::effects` vector contains
//! strings for now, while the checker and all resolved consumers use the
//! canonical names below.  An unknown operation is represented explicitly by
//! `EffectKind::Unknown`; it is never silently treated as pure.

use crate::ast::{Block, Expr, FuncDef, Item, Lit, Stmt};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

/// Finite effect vocabulary used by the 0.2 effect boundary.
///
/// `Unknown` is an analysis result, rather than a declaration that can be
/// used to widen a public API.  Keeping it in the set makes fail-closed
/// propagation cheap and deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectKind {
    FileRead,
    FileWrite,
    Network,
    SharedMutation,
    Blocking,
    Suspend,
    Spawn,
    Unknown,
}

impl EffectKind {
    /// Canonical wire/interface spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileRead => "file.read",
            Self::FileWrite => "file.write",
            Self::Network => "network",
            Self::SharedMutation => "shared.modify",
            Self::Blocking => "blocking",
            Self::Suspend => "suspend",
            Self::Spawn => "spawn",
            Self::Unknown => "unknown",
        }
    }

    pub const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Parse both canonical names and the names used by the existing stdlib
    /// and historical examples.  `io` is intentionally expanded to the two
    /// file effects by [`EffectSet::from_names`].
    pub fn parse(name: &str) -> Option<Self> {
        let normalized = name
            .trim()
            .to_ascii_lowercase()
            .chars()
            .map(|character| {
                if matches!(character, '-' | '_') {
                    '.'
                } else {
                    character
                }
            })
            .collect::<String>();
        match normalized.as_str() {
            "file.read" | "fileread" | "read" | "read.file" | "readfile" => Some(Self::FileRead),
            "file.write" | "filewrite" | "write" | "write.file" | "writefile" => {
                Some(Self::FileWrite)
            }
            "network" | "net" | "io.network" => Some(Self::Network),
            "shared.modify" | "shared.mutate" | "sharedmutation" | "shared" | "mutation" => {
                Some(Self::SharedMutation)
            }
            "blocking" | "block" => Some(Self::Blocking),
            "suspend" | "suspending" | "await" | "async" => Some(Self::Suspend),
            "spawn" | "task.spawn" => Some(Self::Spawn),
            "unknown" | "dynamic" | "ffi" | "foreign" => Some(Self::Unknown),
            _ => None,
        }
    }

    pub const fn known() -> [Self; 7] {
        [
            Self::FileRead,
            Self::FileWrite,
            Self::Network,
            Self::SharedMutation,
            Self::Blocking,
            Self::Suspend,
            Self::Spawn,
        ]
    }
}

impl fmt::Display for EffectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canonical finite set.  `BTreeSet` gives stable interface and diagnostic
/// order, which is required for route receipts and module summaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectSet(BTreeSet<EffectKind>);

impl EffectSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn singleton(effect: EffectKind) -> Self {
        let mut set = Self::new();
        set.insert(effect);
        set
    }

    pub fn insert(&mut self, effect: EffectKind) -> bool {
        self.0.insert(effect)
    }

    pub fn remove(&mut self, effect: EffectKind) -> bool {
        self.0.remove(&effect)
    }

    pub fn contains(&self, effect: EffectKind) -> bool {
        self.0.contains(&effect)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn is_unknown(&self) -> bool {
        self.contains(EffectKind::Unknown)
    }

    pub fn iter(&self) -> impl Iterator<Item = EffectKind> + '_ {
        self.0.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn union_with(&mut self, other: &Self) -> bool {
        let before = self.len();
        self.0.extend(other.0.iter().copied());
        self.len() != before
    }

    pub fn union<I>(&mut self, effects: I) -> bool
    where
        I: IntoIterator<Item = EffectKind>,
    {
        let before = self.len();
        self.0.extend(effects);
        self.len() != before
    }

    /// Whether this set is covered by an API's declared upper bound.
    /// `Unknown` is never covered by a finite declaration.
    pub fn is_subset_of(&self, upper: &Self) -> bool {
        !self.is_unknown() && self.0.is_subset(&upper.0)
    }

    pub fn to_vec(&self) -> Vec<String> {
        self.iter()
            .map(|effect| effect.as_str().to_string())
            .collect()
    }

    /// Parse declaration names.  `io` is retained only as a migration alias;
    /// the resulting summary still contains the two finite file effects.
    pub fn from_names<'a, I>(names: I) -> Result<Self, EffectNameError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut set = Self::new();
        for raw in names {
            let normalized = raw.trim().to_ascii_lowercase();
            if normalized == "io" {
                set.insert(EffectKind::FileRead);
                set.insert(EffectKind::FileWrite);
                continue;
            }
            let Some(effect) = EffectKind::parse(raw) else {
                return Err(EffectNameError(raw.trim().to_string()));
            };
            if effect == EffectKind::Unknown {
                return Err(EffectNameError(raw.trim().to_string()));
            }
            set.insert(effect);
        }
        Ok(set)
    }
}

impl FromIterator<EffectKind> for EffectSet {
    fn from_iter<T: IntoIterator<Item = EffectKind>>(iter: T) -> Self {
        let mut set = Self::new();
        set.0.extend(iter);
        set
    }
}

impl fmt::Display for EffectSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let values = self
            .iter()
            .map(|effect| effect.as_str())
            .collect::<Vec<_>>();
        write!(f, "{{{}}}", values.join(", "))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectNameError(pub String);

impl fmt::Display for EffectNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown effect '{}'; expected one of {}",
            self.0,
            known_effect_names()
        )
    }
}

impl std::error::Error for EffectNameError {}

pub fn known_effect_names() -> String {
    EffectKind::known()
        .into_iter()
        .map(|effect| effect.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A callable's effect summary.  The declaration is optional for private
/// functions because private implementation effects are inferred.  Public
/// summaries can be checked with [`EffectSummary::validate_public`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectSummary {
    pub inferred: EffectSet,
    pub declared: Option<EffectSet>,
    /// Stable source names explaining where each effect came from.
    pub sources: BTreeMap<EffectKind, BTreeSet<String>>,
}

impl EffectSummary {
    pub fn is_pure(&self) -> bool {
        self.inferred.is_empty()
    }

    pub fn validate_public(&self, callable: &str) -> Result<(), String> {
        let Some(declared) = &self.declared else {
            if self.inferred.is_empty() {
                return Ok(());
            }
            return Err(format!(
                "public callable '{}' must declare an effect upper bound; inferred {}",
                callable, self.inferred
            ));
        };
        if self.inferred.is_unknown() {
            return Err(format!(
                "public callable '{}' reaches an unknown effect; declare a concrete effect or remove the dynamic/FFI call",
                callable
            ));
        }
        if !self.inferred.is_subset_of(declared) {
            return Err(format!(
                "public callable '{}' inferred effects {} exceed declared upper bound {}",
                callable, self.inferred, declared
            ));
        }
        Ok(())
    }
}

/// A diagnostic emitted by the effect pass.  The checker adds source spans
/// from the callable declaration; keeping this structure independent lets
/// direct API users inspect the same result without rendering diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDiagnostic {
    pub callable: String,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct EffectAnalysis {
    pub summaries: BTreeMap<String, EffectSummary>,
    pub diagnostics: Vec<EffectDiagnostic>,
    /// Exact checker owner identities for typed call-site consumers. The
    /// source-keyed `summaries` map remains the compatibility-facing API.
    pub(crate) owner_summaries: BTreeMap<String, EffectSummary>,
}

impl EffectAnalysis {
    pub fn summary(&self, callable: &str) -> Option<&EffectSummary> {
        self.summaries.get(callable)
    }

    pub fn summaries_as_names(&self) -> HashMap<String, Vec<String>> {
        self.summaries
            .iter()
            .map(|(name, summary)| (name.clone(), summary.inferred.to_vec()))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct Callable<'a> {
    node_key: String,
    summary_key: String,
    owner: crate::core::resolved::NodeId,
    function: &'a FuncDef,
    public: bool,
    captured_scope: LexicalScope,
}

type LexicalScope = BTreeMap<String, Option<String>>;

#[derive(Debug, Clone, Default)]
struct DirectEffects {
    set: EffectSet,
    sources: BTreeMap<EffectKind, BTreeSet<String>>,
    callees: BTreeSet<String>,
}

impl DirectEffects {
    fn add(&mut self, effect: EffectKind, source: impl Into<String>) {
        self.set.insert(effect);
        self.sources
            .entry(effect)
            .or_default()
            .insert(source.into());
    }
}

/// Infer effects for all function-like items in a file.  The pass is
/// conservative by design: unresolved calls, extern calls, and calls through
/// a value are marked `unknown` and therefore cannot become pure by accident.
pub fn infer_items(items: &[Item]) -> EffectAnalysis {
    infer_items_with_sources(items, None)
}

/// Checker entrypoint which preserves module-qualified roots from the source
/// registry when constructing nested callable owner identities.
pub(crate) fn infer_items_with_sources(
    items: &[Item],
    sources: Option<&crate::span::SourceRegistry>,
) -> EffectAnalysis {
    let qualified_names = sources
        .map(|sources| crate::core::resolved::top_level_function_qualified_names(items, sources));
    let mut callables = Vec::<Callable<'_>>::new();
    let mut top_level_names = HashMap::<String, Vec<String>>::new();
    let mut method_names = HashMap::<String, Vec<String>>::new();
    let mut extern_names = HashSet::new();
    let constructor_names = effect_constructor_names(items);
    for item in items {
        match item {
            Item::Func(function) => {
                let qualified_name = match (sources, qualified_names.as_ref()) {
                    (Some(sources), Some(qualified_names)) => {
                        crate::core::resolved::top_level_function_qualified_name(
                            function,
                            sources,
                            qualified_names,
                        )
                    }
                    _ => function.name.clone(),
                };
                let owner = crate::core::resolved::NodeId(format!("function:{qualified_name}"));
                add_callable(
                    &mut callables,
                    &mut top_level_names,
                    qualified_name,
                    owner,
                    function,
                    LexicalScope::new(),
                );
            }
            Item::Impl(implementation) => {
                for function in &implementation.methods {
                    let key = format!("{}::{}", implementation.type_name, function.name);
                    let impl_name = crate::core::resolved::impl_qualified_name(
                        &implementation.trait_name,
                        &implementation.trait_args,
                        &implementation.type_name,
                    );
                    let owner = crate::core::resolved::impl_method_owner(&impl_name, function);
                    add_callable(
                        &mut callables,
                        &mut method_names,
                        key,
                        owner,
                        function,
                        LexicalScope::new(),
                    );
                }
            }
            Item::Actor(actor) => {
                for function in &actor.methods {
                    let key = format!("{}::{}", actor.name, function.name);
                    let owner = crate::core::resolved::NodeId(format!("function:{key}"));
                    add_callable(
                        &mut callables,
                        &mut method_names,
                        key,
                        owner,
                        function,
                        LexicalScope::new(),
                    );
                }
            }
            Item::ExternBlock(block) => {
                extern_names.extend(block.funcs.iter().map(|function| function.name.clone()));
            }
            _ => {}
        }
    }

    // Nested declarations are latent callable nodes. Register the complete
    // lexical directory before scanning bodies so forward sibling calls and
    // recursive nested calls resolve to stable checker owner identities.
    let roots = callables.clone();
    for callable in &roots {
        register_nested_callables(
            &callable.function.body,
            &callable.owner,
            &callable.captured_scope,
            &mut callables,
        );
    }

    let mut direct = BTreeMap::<String, DirectEffects>::new();
    for callable in &callables {
        let mut effects = DirectEffects::default();
        scan_callable_block(
            &callable.function.body,
            &mut effects,
            &callable.owner,
            &callable.captured_scope,
            &top_level_names,
            &method_names,
            &extern_names,
            &constructor_names,
        );
        direct.insert(callable.node_key.clone(), effects);
    }

    // A monotone union fixpoint handles recursion and mutually recursive
    // helpers without pretending that cycle order is semantic.
    let mut inferred = direct
        .iter()
        .map(|(key, effects)| (key.clone(), effects.set.clone()))
        .collect::<BTreeMap<_, _>>();
    for _ in 0..=callables.len() {
        let mut changed = false;
        for callable in &callables {
            let mut next = inferred
                .get(&callable.node_key)
                .cloned()
                .unwrap_or_default();
            let Some(effects) = direct.get(&callable.node_key) else {
                continue;
            };
            for callee in &effects.callees {
                if let Some(target_effects) = inferred.get(callee) {
                    changed |= next.union_with(target_effects);
                } else {
                    next.insert(EffectKind::Unknown);
                    changed = true;
                }
            }
            changed |= inferred
                .insert(callable.node_key.clone(), next.clone())
                .is_some_and(|old| old != next);
        }
        if !changed {
            break;
        }
    }

    let mut analysis = EffectAnalysis::default();
    for callable in callables {
        let inferred_set = inferred.remove(&callable.node_key).unwrap_or_default();
        let declared = if callable.function.effects.is_empty() {
            None
        } else {
            match EffectSet::from_names(callable.function.effects.iter().map(String::as_str)) {
                Ok(set) => Some(set),
                Err(error) => {
                    analysis.diagnostics.push(EffectDiagnostic {
                        callable: callable.summary_key.clone(),
                        message: error.to_string(),
                    });
                    None
                }
            }
        };
        let summary = EffectSummary {
            inferred: inferred_set,
            declared,
            sources: direct
                .get(&callable.node_key)
                .map(|effects| effects.sources.clone())
                .unwrap_or_default(),
        };
        // A declaration is an upper bound, not a way to hide an effect.  Keep
        // the inferred set authoritative and report the mismatch once.
        if let Some(declared) = &summary.declared {
            if summary.inferred.is_unknown() {
                analysis.diagnostics.push(EffectDiagnostic {
                    callable: callable.summary_key.clone(),
                    message: format!(
                        "callable '{}' reaches an unknown effect and cannot satisfy declared upper bound {}",
                        callable.summary_key, declared
                    ),
                });
            } else if !summary.inferred.is_subset_of(declared) {
                analysis.diagnostics.push(EffectDiagnostic {
                    callable: callable.summary_key.clone(),
                    message: format!(
                        "callable '{}' inferred effects {} exceed declared upper bound {}",
                        callable.summary_key, summary.inferred, declared
                    ),
                });
            }
        }
        // Public declarations with an explicit effect list are validated by
        // default.  Existing stdlib/public code with no declaration remains
        // compatible until the new surface declaration is selected; callers
        // can use `validate_public_summaries` for a strict module boundary.
        if callable.public && !callable.function.effects.is_empty() {
            if let Err(message) = summary.validate_public(&callable.summary_key) {
                analysis.diagnostics.push(EffectDiagnostic {
                    callable: callable.summary_key.clone(),
                    message,
                });
            }
        }
        analysis
            .owner_summaries
            .insert(callable.owner.0.clone(), summary.clone());
        analysis.summaries.insert(callable.summary_key, summary);
    }
    analysis
}

/// Names that the checker registers as value constructors. Keep this directory
/// tied to the corresponding declared type identity: records and aliases are
/// not callable constructors, and duplicate variant names stay unresolved.
fn effect_constructor_names(items: &[Item]) -> HashSet<String> {
    let mut counts = HashMap::<String, usize>::new();
    for item in items {
        let Item::Type(definition) = item else {
            continue;
        };
        match &definition.kind {
            crate::ast::TypeDefKind::Newtype(_) => {
                *counts.entry(definition.name.clone()).or_default() += 1;
            }
            crate::ast::TypeDefKind::Enum(variants)
                if !matches!(
                    definition.meta.origin,
                    crate::ast::AstOrigin::Desugared("flow_matrix.fault_nominal")
                ) =>
            {
                for variant in variants {
                    *counts.entry(variant.name.clone()).or_default() += 1;
                }
            }
            crate::ast::TypeDefKind::Alias(_)
            | crate::ast::TypeDefKind::Record(_)
            | crate::ast::TypeDefKind::Enum(_)
            | crate::ast::TypeDefKind::Union(_) => {}
        }
    }
    counts
        .into_iter()
        .filter_map(|(name, count)| (count == 1).then_some(name))
        .collect()
}

/// Strict public-boundary check for module/interface builders.  This is kept
/// separate from `infer_items` so existing source files can migrate without
/// reviving the removed `with` syntax.
pub fn validate_public_summaries(analysis: &EffectAnalysis) -> Vec<EffectDiagnostic> {
    analysis
        .summaries
        .iter()
        .filter_map(|(name, summary)| {
            summary
                .validate_public(name)
                .err()
                .map(|message| EffectDiagnostic {
                    callable: name.clone(),
                    message,
                })
        })
        .collect()
}

fn add_callable<'a>(
    callables: &mut Vec<Callable<'a>>,
    names: &mut HashMap<String, Vec<String>>,
    summary_key: String,
    owner: crate::core::resolved::NodeId,
    function: &'a FuncDef,
    captured_scope: LexicalScope,
) {
    let node_key = owner.0.clone();
    names
        .entry(function.name.clone())
        .or_default()
        .push(node_key.clone());
    callables.push(Callable {
        node_key,
        summary_key,
        owner,
        function,
        public: function.pub_,
        captured_scope,
    });
}

fn register_nested_callables<'a>(
    block: &'a Block,
    owner: &crate::core::resolved::NodeId,
    inherited_scope: &LexicalScope,
    callables: &mut Vec<Callable<'a>>,
) {
    let mut local = BTreeMap::<String, Option<String>>::new();
    let mut declarations = Vec::new();
    for statement in block {
        let Stmt::Func(function) = statement.unlocated() else {
            continue;
        };
        let nested_owner = crate::core::resolved::nested_function_owner(owner, function);
        match local.get(&function.name) {
            Some(_) => {
                local.insert(function.name.clone(), None);
            }
            None => {
                local.insert(function.name.clone(), Some(nested_owner.0.clone()));
            }
        }
        declarations.push((function, nested_owner));
    }
    let mut scope = inherited_scope.clone();
    scope.extend(local);

    for (function, nested_owner) in &declarations {
        callables.push(Callable {
            node_key: nested_owner.0.clone(),
            summary_key: nested_owner.0.clone(),
            owner: nested_owner.clone(),
            function,
            public: function.pub_,
            captured_scope: scope.clone(),
        });
    }
    for (function, nested_owner) in declarations {
        register_nested_callables(&function.body, &nested_owner, &scope, callables);
    }
    for statement in block {
        if !matches!(statement.unlocated(), Stmt::Func(_)) {
            register_nested_in_statement(statement, owner, &scope, callables);
        }
    }
}

fn register_nested_in_statement<'a>(
    statement: &'a Stmt,
    owner: &crate::core::resolved::NodeId,
    scope: &LexicalScope,
    callables: &mut Vec<Callable<'a>>,
) {
    match statement.unlocated() {
        Stmt::If { cond, then_, else_ } => {
            register_nested_in_expr(cond, owner, scope, callables);
            register_nested_callables(then_, owner, scope, callables);
            if let Some(other) = else_ {
                register_nested_callables(other, owner, scope, callables);
            }
        }
        Stmt::IfLet {
            init, then_, else_, ..
        } => {
            register_nested_in_expr(init, owner, scope, callables);
            register_nested_callables(then_, owner, scope, callables);
            if let Some(other) = else_ {
                register_nested_callables(other, owner, scope, callables);
            }
        }
        Stmt::While { cond, body } => {
            register_nested_in_expr(cond, owner, scope, callables);
            register_nested_callables(body, owner, scope, callables);
        }
        Stmt::WhileLet { init, body, .. } => {
            register_nested_in_expr(init, owner, scope, callables);
            register_nested_callables(body, owner, scope, callables);
        }
        Stmt::For { iterable, body, .. } => {
            register_nested_in_expr(iterable, owner, scope, callables);
            register_nested_callables(body, owner, scope, callables);
        }
        Stmt::Loop(body)
        | Stmt::Block(body)
        | Stmt::Arena(body)
        | Stmt::Unsafe(body)
        | Stmt::IeeeFloat(body)
        | Stmt::Defer(body)
        | Stmt::OnFailure(body)
        | Stmt::Parasteps(body) => register_nested_callables(body, owner, scope, callables),
        Stmt::Pinned { expr, body, .. } => {
            register_nested_in_expr(expr, owner, scope, callables);
            register_nested_callables(body, owner, scope, callables);
        }
        Stmt::Let {
            init: Some(expr), ..
        }
        | Stmt::SharedLet { init: expr, .. }
        | Stmt::Expr(expr)
        | Stmt::Return(Some(expr))
        | Stmt::Break(Some(expr))
        | Stmt::Drop(expr)
        | Stmt::Requires(expr, _)
        | Stmt::Ensures(expr, _)
        | Stmt::Invariant(expr, _) => register_nested_in_expr(expr, owner, scope, callables),
        Stmt::Assign { target, value } => {
            register_nested_in_expr(target, owner, scope, callables);
            register_nested_in_expr(value, owner, scope, callables);
        }
        Stmt::Math(expressions) => {
            for expression in expressions {
                register_nested_in_expr(expression, owner, scope, callables);
            }
        }
        Stmt::Located { .. } => unreachable!("Stmt::unlocated returned Located"),
        Stmt::Func(_)
        | Stmt::Let { init: None, .. }
        | Stmt::Return(None)
        | Stmt::Break(None)
        | Stmt::Continue
        | Stmt::Ellipsis => {}
    }
}

fn register_nested_in_expr<'a>(
    expression: &'a Expr,
    owner: &crate::core::resolved::NodeId,
    scope: &LexicalScope,
    callables: &mut Vec<Callable<'a>>,
) {
    match expression.unlocated() {
        Expr::Block(body) | Expr::Arena(body) => {
            register_nested_callables(body, owner, scope, callables)
        }
        Expr::If { cond, then_, else_ } => {
            register_nested_in_expr(cond, owner, scope, callables);
            register_nested_callables(then_, owner, scope, callables);
            if let Some(other) = else_ {
                register_nested_callables(other, owner, scope, callables);
            }
        }
        Expr::Lambda { body, .. } => register_nested_callables(body, owner, scope, callables),
        Expr::Call(callee, arguments) => {
            register_nested_in_expr(callee, owner, scope, callables);
            for argument in arguments {
                register_nested_in_expr(argument, owner, scope, callables);
            }
        }
        Expr::Binary(_, left, right) => {
            register_nested_in_expr(left, owner, scope, callables);
            register_nested_in_expr(right, owner, scope, callables);
        }
        Expr::Unary(_, inner)
        | Expr::Try(inner)
        | Expr::OptionalChain(inner, _)
        | Expr::Spawn(inner)
        | Expr::Await(inner)
        | Expr::TypeOf(inner)
        | Expr::Old(inner)
        | Expr::QuoteInterpolate(inner)
        | Expr::NamedArg(_, inner)
        | Expr::Cast(inner, _) => register_nested_in_expr(inner, owner, scope, callables),
        Expr::Field(base, _) | Expr::TupleIndex(base, _) => {
            register_nested_in_expr(base, owner, scope, callables)
        }
        Expr::Index(base, index) => {
            register_nested_in_expr(base, owner, scope, callables);
            register_nested_in_expr(index, owner, scope, callables);
        }
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            for item in items {
                register_nested_in_expr(item, owner, scope, callables);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            register_nested_in_expr(expr, owner, scope, callables);
            register_nested_in_expr(iter, owner, scope, callables);
            if let Some(guard) = guard {
                register_nested_in_expr(guard, owner, scope, callables);
            }
        }
        Expr::Match(subject, arms) => {
            register_nested_in_expr(subject, owner, scope, callables);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    register_nested_in_expr(guard, owner, scope, callables);
                }
                register_nested_in_expr(&arm.body, owner, scope, callables);
            }
        }
        Expr::Record { fields, rest, .. } => {
            for field in fields {
                register_nested_in_expr(&field.value, owner, scope, callables);
            }
            if let Some(rest) = rest {
                register_nested_in_expr(rest, owner, scope, callables);
            }
        }
        Expr::SliceExpr { target, start, end } => {
            register_nested_in_expr(target, owner, scope, callables);
            if let Some(start) = start {
                register_nested_in_expr(start, owner, scope, callables);
            }
            if let Some(end) = end {
                register_nested_in_expr(end, owner, scope, callables);
            }
        }
        Expr::Turbofish(_, _, arguments) => {
            for argument in arguments {
                register_nested_in_expr(argument, owner, scope, callables);
            }
        }
        Expr::MapLiteral { entries } => {
            for (key, value) in entries {
                register_nested_in_expr(key, owner, scope, callables);
                register_nested_in_expr(value, owner, scope, callables);
            }
        }
        // The checker does not finalize declarations inside these opaque
        // compile-time forms, matching the resolved syntax catalog.
        Expr::Quote(_)
        | Expr::Comptime(_)
        | Expr::Literal(_)
        | Expr::Ident(_)
        | Expr::TypeInfo(_) => {}
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

struct ScanEnv<'a> {
    owner: &'a crate::core::resolved::NodeId,
    scope: &'a LexicalScope,
    top_level_names: &'a HashMap<String, Vec<String>>,
    method_names: &'a HashMap<String, Vec<String>>,
    extern_names: &'a HashSet<String>,
    constructor_names: &'a HashSet<String>,
}

fn is_declared_user_constructor(name: &str, env: &ScanEnv<'_>) -> bool {
    env.constructor_names.contains(name)
        && !env.scope.contains_key(name)
        && !env.top_level_names.contains_key(name)
        && !env.method_names.contains_key(name)
        && !env.extern_names.contains(name)
}

fn lexical_scope_for_block(
    block: &Block,
    owner: &crate::core::resolved::NodeId,
    inherited_scope: &LexicalScope,
) -> LexicalScope {
    let mut local = BTreeMap::<String, Option<String>>::new();
    for statement in block {
        let Stmt::Func(function) = statement.unlocated() else {
            continue;
        };
        let nested_owner = crate::core::resolved::nested_function_owner(owner, function);
        match local.get(&function.name) {
            Some(_) => {
                local.insert(function.name.clone(), None);
            }
            None => {
                local.insert(function.name.clone(), Some(nested_owner.0));
            }
        }
    }
    let mut scope = inherited_scope.clone();
    scope.extend(local);
    scope
}

fn scan_callable_block(
    block: &Block,
    effects: &mut DirectEffects,
    owner: &crate::core::resolved::NodeId,
    inherited_scope: &LexicalScope,
    top_level_names: &HashMap<String, Vec<String>>,
    method_names: &HashMap<String, Vec<String>>,
    extern_names: &HashSet<String>,
    constructor_names: &HashSet<String>,
) {
    let scope = lexical_scope_for_block(block, owner, inherited_scope);
    let env = ScanEnv {
        owner,
        scope: &scope,
        top_level_names,
        method_names,
        extern_names,
        constructor_names,
    };
    for statement in block {
        scan_stmt(statement, effects, &env);
    }
}

fn scan_stmt(statement: &Stmt, effects: &mut DirectEffects, env: &ScanEnv<'_>) {
    match statement.unlocated() {
        Stmt::Located { .. } => unreachable!(),
        Stmt::Let { init, .. } => {
            if let Some(init) = init {
                scan_expr(init, effects, env);
            }
        }
        Stmt::Return(value) | Stmt::Break(value) => {
            if let Some(value) = value {
                scan_expr(value, effects, env);
            }
        }
        Stmt::Expr(expr)
        | Stmt::Drop(expr)
        | Stmt::Requires(expr, _)
        | Stmt::Ensures(expr, _)
        | Stmt::Invariant(expr, _) => scan_expr(expr, effects, env),
        Stmt::If { cond, then_, else_ } => {
            scan_expr(cond, effects, env);
            scan_callable_block(
                then_,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
            if let Some(other) = else_ {
                scan_callable_block(
                    other,
                    effects,
                    env.owner,
                    env.scope,
                    env.top_level_names,
                    env.method_names,
                    env.extern_names,
                    env.constructor_names,
                );
            }
        }
        Stmt::IfLet {
            init, then_, else_, ..
        } => {
            scan_expr(init, effects, env);
            scan_callable_block(
                then_,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
            if let Some(other) = else_ {
                scan_callable_block(
                    other,
                    effects,
                    env.owner,
                    env.scope,
                    env.top_level_names,
                    env.method_names,
                    env.extern_names,
                    env.constructor_names,
                );
            }
        }
        Stmt::While { cond, body } => {
            scan_expr(cond, effects, env);
            scan_callable_block(
                body,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
        }
        Stmt::WhileLet { init, body, .. } => {
            scan_expr(init, effects, env);
            scan_callable_block(
                body,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
        }
        Stmt::Loop(body)
        | Stmt::Block(body)
        | Stmt::Arena(body)
        | Stmt::Unsafe(body)
        | Stmt::IeeeFloat(body)
        | Stmt::Defer(body)
        | Stmt::OnFailure(body)
        | Stmt::Parasteps(body) => {
            if matches!(statement.unlocated(), Stmt::Parasteps(_)) {
                effects.add(EffectKind::Spawn, "parasteps");
            }
            scan_callable_block(
                body,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
        }
        Stmt::For { iterable, body, .. } => {
            scan_expr(iterable, effects, env);
            scan_callable_block(
                body,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
        }
        Stmt::Assign { target, value } => {
            scan_expr(target, effects, env);
            scan_expr(value, effects, env);
        }
        Stmt::SharedLet { init, .. } => {
            effects.add(EffectKind::SharedMutation, "shared binding");
            scan_expr(init, effects, env);
        }
        Stmt::Pinned { expr, body, .. } => {
            effects.add(EffectKind::Unknown, "pinned/FFI boundary");
            scan_expr(expr, effects, env);
            scan_callable_block(
                body,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
        }
        Stmt::Func(_) => {}
        Stmt::Continue | Stmt::Math(_) | Stmt::Ellipsis => {}
    }
}

fn scan_expr(expression: &Expr, effects: &mut DirectEffects, env: &ScanEnv<'_>) {
    match expression.unlocated() {
        Expr::Located { .. } => unreachable!(),
        Expr::Call(callee, arguments) => {
            match callee.unlocated() {
                Expr::Ident(name) => {
                    if is_declared_user_constructor(name, env) {
                        // A declared newtype or enum variant constructor is a
                        // known pure data operation.
                    } else {
                        let builtin_facets = builtin_effects(name);
                        if !builtin_facets.is_empty() {
                            for effect in builtin_facets.iter() {
                                effects.add(effect, format!("builtin '{name}'"));
                            }
                        } else if crate::core::builtins::is_builtin_callable(name)
                            || crate::core::builtins::is_language_intrinsic_callable(name)
                            || crate::core::builtins::is_language_constructor(name)
                        {
                            // Known pure builtin/intrinsic.
                        } else if let Some(target) = env.scope.get(name) {
                            if let Some(target) = target {
                                effects.callees.insert(target.clone());
                            } else {
                                effects.add(
                                    EffectKind::Unknown,
                                    format!("ambiguous nested call '{name}'"),
                                );
                            }
                        } else if env.extern_names.contains(name) {
                            effects.add(EffectKind::Unknown, format!("extern '{name}'"));
                        } else if let Some(targets) = env.top_level_names.get(name) {
                            effects.callees.extend(targets.iter().cloned());
                        } else if let Some(targets) = env.method_names.get(name) {
                            // Preserve the checker pass's historical support for
                            // unqualified method calls inside an impl/actor body.
                            effects.callees.extend(targets.iter().cloned());
                        } else {
                            effects.add(EffectKind::Unknown, format!("dynamic call '{name}'"));
                        }
                    }
                }
                Expr::Field(_, method) => {
                    if let Some(targets) = env.method_names.get(method) {
                        effects.callees.extend(targets.iter().cloned());
                    } else if is_pure_builtin_method(method) {
                        // Closed observation/extraction methods are pure.
                    } else {
                        effects.add(EffectKind::Unknown, format!("dynamic method '{method}'"));
                    }
                }
                _ => effects.add(EffectKind::Unknown, "dynamic call"),
            }
            scan_expr(callee, effects, env);
            for argument in arguments {
                scan_expr(argument, effects, env);
            }
        }
        Expr::Spawn(inner) => {
            effects.add(EffectKind::Spawn, "spawn expression");
            scan_expr(inner, effects, env);
        }
        Expr::Await(inner) => {
            effects.add(EffectKind::Suspend, "await expression");
            scan_expr(inner, effects, env);
        }
        Expr::Lambda { body, .. } => scan_callable_block(
            body,
            effects,
            env.owner,
            env.scope,
            env.top_level_names,
            env.method_names,
            env.extern_names,
            env.constructor_names,
        ),
        Expr::If { cond, then_, else_ } => {
            scan_expr(cond, effects, env);
            scan_callable_block(
                then_,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
            if let Some(other) = else_ {
                scan_callable_block(
                    other,
                    effects,
                    env.owner,
                    env.scope,
                    env.top_level_names,
                    env.method_names,
                    env.extern_names,
                    env.constructor_names,
                );
            }
        }
        Expr::Block(body) | Expr::Arena(body) | Expr::Quote(body) | Expr::Comptime(body) => {
            scan_callable_block(
                body,
                effects,
                env.owner,
                env.scope,
                env.top_level_names,
                env.method_names,
                env.extern_names,
                env.constructor_names,
            );
        }
        Expr::Binary(_, left, right) => {
            scan_expr(left, effects, env);
            scan_expr(right, effects, env);
        }
        Expr::Unary(_, inner)
        | Expr::Try(inner)
        | Expr::OptionalChain(inner, _)
        | Expr::TypeOf(inner)
        | Expr::Old(inner)
        | Expr::QuoteInterpolate(inner)
        | Expr::NamedArg(_, inner)
        | Expr::Cast(inner, _) => scan_expr(inner, effects, env),
        Expr::Field(base, _) | Expr::TupleIndex(base, _) => scan_expr(base, effects, env),
        Expr::Index(base, index) => {
            scan_expr(base, effects, env);
            scan_expr(index, effects, env);
        }
        Expr::Tuple(values) | Expr::List(values) | Expr::SetLiteral(values) => {
            for value in values {
                scan_expr(value, effects, env);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            scan_expr(expr, effects, env);
            scan_expr(iter, effects, env);
            if let Some(guard) = guard {
                scan_expr(guard, effects, env);
            }
        }
        Expr::Match(subject, arms) => {
            scan_expr(subject, effects, env);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    scan_expr(guard, effects, env);
                }
                scan_expr(&arm.body, effects, env);
            }
        }
        Expr::Record { fields, rest, .. } => {
            for field in fields {
                scan_expr(&field.value, effects, env);
            }
            if let Some(rest) = rest {
                scan_expr(rest, effects, env);
            }
        }
        Expr::SliceExpr { target, start, end } => {
            scan_expr(target, effects, env);
            if let Some(start) = start {
                scan_expr(start, effects, env);
            }
            if let Some(end) = end {
                scan_expr(end, effects, env);
            }
        }
        Expr::Turbofish(name, _, arguments) => {
            let builtin_facets = builtin_effects(name);
            if !builtin_facets.is_empty() {
                for effect in builtin_facets.iter() {
                    effects.add(effect, format!("builtin '{name}'"));
                }
            } else if crate::core::builtins::is_builtin_callable(name)
                || crate::core::builtins::is_language_intrinsic_callable(name)
                || crate::core::builtins::is_language_constructor(name)
            {
                // This name belongs to the closed checker builtin catalog.
                // Its finite effect summary is empty; effectful builtins take
                // the branch above and retain their declared facets.
            } else if let Some(target) = env.scope.get(name) {
                if let Some(target) = target {
                    effects.callees.insert(target.clone());
                } else {
                    effects.add(
                        EffectKind::Unknown,
                        format!("ambiguous nested generic call '{name}'"),
                    );
                }
            } else if env.extern_names.contains(name) {
                effects.add(EffectKind::Unknown, format!("extern generic call '{name}'"));
            } else if let Some(targets) = env.top_level_names.get(name) {
                effects.callees.extend(targets.iter().cloned());
            } else {
                effects.add(
                    EffectKind::Unknown,
                    format!("dynamic generic call '{name}'"),
                );
            }
            for argument in arguments {
                scan_expr(argument, effects, env);
            }
        }
        Expr::Literal(Lit::FString(parts)) => {
            for part in parts {
                if let crate::ast::FStringPart::Interp(expr) = part {
                    scan_expr(expr, effects, env);
                }
            }
        }
        Expr::MapLiteral { entries } => {
            for (key, value) in entries {
                scan_expr(key, effects, env);
                scan_expr(value, effects, env);
            }
        }
        Expr::Literal(_) | Expr::Ident(_) | Expr::TypeInfo(_) => {}
    }
}

/// Methods resolved by the checker as closed, effect-free Option/Result and
/// List operations.  Callback-bearing methods (`map`, `and_then`, `map_err`)
/// are intentionally excluded because their closure bodies carry their own
/// effect summary and must be analyzed through the normal call path.  List
/// transforms consume/return values but do not mutate shared state, so their
/// ownership permission is not an effect facet.
pub(crate) fn is_pure_builtin_method(name: &str) -> bool {
    matches!(
        name,
        "unwrap"
            | "expect"
            | "unwrap_or"
            | "is_some"
            | "is_none"
            | "is_ok"
            | "is_err"
            | "len"
            | "reverse"
            | "concat"
    )
}

/// Classify known builtins.  This table intentionally names operation
/// semantics rather than implementation modules; stdlib wrappers inherit the
/// effects of their calls through normal interprocedural propagation.
pub fn builtin_effect(name: &str) -> Option<EffectKind> {
    match name {
        // File read operations.
        "file_exists"
        | "read_file"
        | "fs_exists"
        | "fs_read"
        | "fs_read_lines"
        | "fs_file_size"
        | "read_file_partial"
        | "read_file_bytes"
        | "read_file_guarded"
        | "read_lines_json"
        | "read_lines_json_builtin"
        | "read_lines_each"
        | "listdir"
        | "is_dir"
        | "is_file"
        | "walk_dir"
        | "fs_listdir"
        | "fs_walk_dir"
        | "file_stat" => Some(EffectKind::FileRead),
        // File writes.
        "write_file"
        | "write_file_guarded"
        | "fs_write"
        | "fs_write_lines"
        | "mkdir_p"
        | "remove_file"
        | "append_file"
        | "write_file_bytes" => Some(EffectKind::FileWrite),
        // Networking.  Connection establishment and transfer can also block;
        // the blocking facet is added below by `builtin_effects`.
        "socket" | "connect" | "bind" | "listen" | "accept" | "send" | "recv" | "close_fd"
        | "tcp_socket" | "tcp_connect" | "tcp_listen" | "tcp_accept" | "tcp_send" | "tcp_recv"
        | "fetch" | "fetch_post" | "http_get" | "http_get_guarded" | "http_post" => {
            Some(EffectKind::Network)
        }
        // Shared mutable state and synchronization.
        "atomic_i32_store"
        | "atomic_i32_fetch_add"
        | "atomic_i32_compare_exchange"
        | "atomic_i64_store"
        | "atomic_i64_fetch_add"
        | "atomic_i64_compare_exchange"
        | "atomic_bool_store"
        | "atomic_bool_compare_exchange"
        | "mutex_set"
        | "mutex_unlock"
        | "map_set"
        | "set_env"
        // Capability issuance and channel/session/actor operations mutate a
        // host-owned/shared resource. They must not disappear behind the
        // `is_builtin_callable` pure fallback in `scan_expr`.
        | "make_token"
        | "channel_new"
        | "channel_send"
        | "channel_drop"
        | "token_channel_new"
        | "token_channel_send"
        | "session_open"
        | "session_pair"
        | "session_send"
        | "session_close"
        | "mutex_new"
        | "mutex_get"
        | "mutex_drop"
        | "flow_bump_epoch"
        | "actor_set_mailbox_depth"
        | "actor_set_max_children"
        | "actor_spawn_count"
        | "actor_max_children"
        | "actor_mailbox_depth"
        | "actor_is_faulted"
        | "actor_is_muted" => Some(EffectKind::SharedMutation),
        // Operations which may wait.
        "input"
        | "input_line"
        | "try_input_line"
        | "input_int"
        | "input_float"
        | "input_bool"
        | "sleep"
        | "sleep_ms"
        | "sleep_until"
        | "mutex_lock"
        | "channel_recv"
        | "channel_try_recv"
        | "token_channel_recv"
        | "session_recv"
        | "exec"
        | "exec_safe"
        | "exec_pipe" => {
            Some(EffectKind::Blocking)
        }
        // Spawning is explicit in source or through these task/actor builtins.
        "spawn_detached" | "broadcast" => Some(EffectKind::Spawn),
        // `println` mutates a process-global stream.  It is represented by the
        // existing finite shared-mutation bucket until a dedicated stdout
        // effect is promoted into the vocabulary.
        "println" | "print" | "eprintln" | "print_line" | "print_raw" | "print_format"
        | "print_err" => Some(EffectKind::SharedMutation),
        // Clock, RNG, environment reads and opaque runtime handles are
        // external observations not covered by the six user-facing classes.
        "now"
        | "now_ms"
        | "timestamp"
        | "timestamp_ms"
        | "random"
        | "random_int"
        | "random_normal" | "random_uniform" | "random_exponential" | "random_bernoulli"
        | "getenv" | "get_var" | "get_var_or" | "has_var" | "get_int" | "get_float"
        | "arg_count"
        | "first_arg"
        | "cli_args"
        | "args"
        | "get_env_guarded"
        // Process termination, dynamic source evaluation and unsafe shadow
        // memory are intentionally opaque to the finite effect lattice.
        | "exit"
        | "lexer"
        | "mms_parse"
        | "ast_eval"
        | "ast_dump"
        | "shadow_alloc"
        | "shadow_tag"
        | "shadow_check"
        | "shadow_free"
        | "inject_fault"
        | "test_sandbox" => Some(EffectKind::Unknown),
        _ => None,
    }
}

/// Return all facets of a builtin.  A call may have more than one effect
/// (network operations can block, and `fetch` is both network and blocking).
pub fn builtin_effects(name: &str) -> EffectSet {
    let mut set = EffectSet::new();
    if let Some(effect) = builtin_effect(name) {
        set.insert(effect);
    }
    if matches!(
        name,
        "socket"
            | "connect"
            | "listen"
            | "accept"
            | "send"
            | "recv"
            | "tcp_socket"
            | "tcp_connect"
            | "tcp_listen"
            | "tcp_accept"
            | "tcp_send"
            | "tcp_recv"
            | "fetch"
            | "fetch_post"
            | "http_get"
            | "http_get_guarded"
            | "http_post"
    ) {
        set.insert(EffectKind::Network);
    }
    if matches!(
        name,
        "accept"
            | "recv"
            | "tcp_accept"
            | "tcp_recv"
            | "fetch"
            | "fetch_post"
            | "http_get"
            | "http_get_guarded"
            | "http_post"
    ) {
        set.insert(EffectKind::Blocking);
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{
        AstNodeMeta, AstOrigin, Block, ExternBlock, ExternFunc, FuncDef, Item, Type, TypeDef,
        TypeDefKind, Variant, VariantPayload,
    };

    fn function(name: &str, body: Block) -> FuncDef {
        FuncDef {
            meta: AstNodeMeta::synthetic(AstOrigin::User),
            name: name.into(),
            pub_: false,
            params: Vec::new(),
            ret: None,
            body,
            where_clause: Vec::new(),
            generics: Vec::new(),
            effects: Vec::new(),
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            has_requires: false,
            has_ensures: false,
            has_mutate_params: false,
        }
    }

    fn call(name: &str) -> Stmt {
        Stmt::Expr(Expr::Call(
            Box::new(Expr::Ident(name.to_string())),
            Vec::new(),
        ))
    }

    fn type_definition(name: &str, kind: TypeDefKind) -> TypeDef {
        TypeDef {
            meta: AstNodeMeta::synthetic(AstOrigin::User),
            name: name.into(),
            pub_: false,
            kind,
            generics: Vec::new(),
            derives: Vec::new(),
            attributes: Vec::new(),
        }
    }

    #[test]
    fn finite_set_is_deterministic_and_unknown_is_not_declared() {
        let set = EffectSet::from_names(["Network", "blocking"]).unwrap();
        assert_eq!(set.to_vec(), ["network", "blocking"]);
        assert!(!EffectSet::singleton(EffectKind::Unknown).is_subset_of(&set));
        assert!(EffectSet::from_names(["unknown"]).is_err());
    }

    #[test]
    fn propagation_reaches_file_and_network_through_recursive_helpers() {
        let source = vec![
            Item::Func(function(
                "read",
                vec![Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident("read_file".into())),
                    vec![],
                ))],
            )),
            Item::Func(function(
                "fetcher",
                vec![Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident("fetch".into())),
                    vec![],
                ))],
            )),
            Item::Func(function(
                "caller",
                vec![Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident("read".into())),
                    vec![],
                ))],
            )),
        ];
        let analysis = infer_items(&source);
        assert!(analysis
            .summary("read")
            .unwrap()
            .inferred
            .contains(EffectKind::FileRead));
        assert!(analysis
            .summary("fetcher")
            .unwrap()
            .inferred
            .contains(EffectKind::Network));
        assert!(analysis
            .summary("fetcher")
            .unwrap()
            .inferred
            .contains(EffectKind::Blocking));
        assert!(analysis
            .summary("caller")
            .unwrap()
            .inferred
            .contains(EffectKind::FileRead));
    }

    #[test]
    fn dynamic_and_extern_calls_are_unknown() {
        let function = function(
            "f",
            vec![Stmt::Expr(Expr::Call(
                Box::new(Expr::Ident("callback".into())),
                vec![],
            ))],
        );
        let analysis = infer_items(&[Item::Func(function)]);
        assert!(analysis.summary("f").unwrap().inferred.is_unknown());
    }

    #[test]
    fn declared_enum_and_newtype_constructors_are_pure_but_unknown_calls_stay_unknown() {
        let shape = type_definition(
            "Shape",
            TypeDefKind::Enum(vec![Variant {
                meta: AstNodeMeta::synthetic(AstOrigin::User),
                name: "Circle".into(),
                payload: Some(VariantPayload::Tuple(vec![Type::Name(
                    "f64".into(),
                    vec![],
                )])),
            }]),
        );
        let user_id = type_definition(
            "UserId",
            TypeDefKind::Newtype(Type::Name("i32".into(), vec![])),
        );
        let constructors = function(
            "kernel_constructors",
            vec![
                Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident("Circle".into())),
                    vec![Expr::Literal(Lit::Float(2.0))],
                )),
                Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident("UserId".into())),
                    vec![Expr::Literal(Lit::Int(7))],
                )),
            ],
        );
        let unknown = function("unknown_target", vec![call("not_declared")]);

        let analysis = infer_items(&[
            Item::Type(shape),
            Item::Type(user_id),
            Item::Func(constructors),
            Item::Func(unknown),
        ]);
        assert!(analysis
            .summary("kernel_constructors")
            .expect("constructor summary")
            .is_pure());
        assert!(analysis
            .summary("unknown_target")
            .expect("unknown call summary")
            .inferred
            .is_unknown());
    }

    #[test]
    fn generic_session_open_keeps_its_known_finite_effect() {
        let open = function(
            "open_session",
            vec![Stmt::Expr(Expr::Turbofish(
                "session_open".into(),
                vec![Type::Name("Half".into(), vec![])],
                vec![],
            ))],
        );
        let analysis = infer_items(&[Item::Func(open)]);
        let summary = analysis
            .summary("open_session")
            .expect("session_open summary");

        assert_eq!(summary.inferred.to_vec(), ["shared.modify"]);
        assert!(!summary.inferred.is_unknown());
    }

    #[test]
    fn captured_pure_nested_helper_has_an_exact_latent_summary() {
        let helper = function(
            "captured_helper",
            vec![Stmt::Return(Some(Expr::Ident("captured_value".into())))],
        );
        let helper_owner = crate::core::resolved::nested_function_owner(
            &crate::core::resolved::NodeId("function:main".into()),
            &helper,
        );
        let main = function("main", vec![Stmt::Func(helper), call("captured_helper")]);

        let analysis = infer_items(&[Item::Func(main)]);
        assert!(analysis.summary("main").unwrap().is_pure());
        assert!(analysis
            .owner_summaries
            .get(&helper_owner.0)
            .expect("nested helper summary is keyed by checker owner")
            .is_pure());
    }

    #[test]
    fn nested_lexical_shadowing_selects_the_exact_owner_and_keeps_bodies_latent() {
        let global_helper = function(
            "helper",
            vec![Stmt::Expr(Expr::Call(
                Box::new(Expr::Ident("read_file".into())),
                vec![Expr::Literal(Lit::String("path".into()))],
            ))],
        );
        let local_helper = function("helper", vec![]);
        let local_owner = crate::core::resolved::nested_function_owner(
            &crate::core::resolved::NodeId("function:main".into()),
            &local_helper,
        );
        let main = function("main", vec![Stmt::Func(local_helper), call("helper")]);

        let analysis = infer_items(&[Item::Func(global_helper), Item::Func(main)]);
        assert!(analysis
            .summary("helper")
            .unwrap()
            .inferred
            .contains(EffectKind::FileRead));
        assert!(analysis.summary("main").unwrap().is_pure());
        assert!(analysis
            .owner_summaries
            .get(&local_owner.0)
            .expect("lexical helper summary is keyed by its exact owner")
            .is_pure());
    }

    #[test]
    fn nearest_nested_scope_shadows_enclosing_function_without_charging_unused_body() {
        let outer_helper = function(
            "helper",
            vec![Stmt::Expr(Expr::Call(
                Box::new(Expr::Ident("write_file".into())),
                Vec::new(),
            ))],
        );
        let inner_helper = function("helper", Vec::new());
        let outer = function(
            "outer",
            vec![
                Stmt::Func(outer_helper.clone()),
                Stmt::Func(function(
                    "inner",
                    vec![Stmt::Func(inner_helper.clone()), call("helper")],
                )),
                call("inner"),
            ],
        );
        let outer_owner = crate::core::resolved::nested_function_owner(
            &crate::core::resolved::NodeId("function:main".into()),
            &outer,
        );
        let outer_helper_owner =
            crate::core::resolved::nested_function_owner(&outer_owner, &outer_helper);
        let inner = match outer.body.get(1).map(Stmt::unlocated) {
            Some(Stmt::Func(inner)) => inner,
            _ => unreachable!(),
        };
        let inner_owner = crate::core::resolved::nested_function_owner(&outer_owner, inner);
        let inner_helper_owner =
            crate::core::resolved::nested_function_owner(&inner_owner, &inner_helper);
        let main = function("main", vec![Stmt::Func(outer), call("outer")]);

        let analysis = infer_items(&[Item::Func(main)]);
        assert!(analysis.summary("main").unwrap().is_pure());
        assert!(analysis
            .owner_summaries
            .get(&outer_helper_owner.0)
            .expect("outer helper summary")
            .inferred
            .contains(EffectKind::FileWrite));
        assert!(analysis
            .owner_summaries
            .get(&inner_helper_owner.0)
            .expect("nearest lexical helper summary")
            .is_pure());
    }

    #[test]
    fn nested_extern_and_dynamic_calls_remain_unknown() {
        let external = ExternBlock {
            meta: AstNodeMeta::synthetic(AstOrigin::User),
            abi: "C".into(),
            funcs: vec![ExternFunc {
                meta: AstNodeMeta::synthetic(AstOrigin::User),
                name: "foreign".into(),
                params: Vec::new(),
                ret: None,
                requires: None,
                ensures: None,
                variadic: false,
                no_panic: false,
                returns_errno: false,
            }],
            explicit_errno_funcs: Vec::new(),
            no_panic: false,
            unsafe_: false,
            returns_errno: false,
        };
        let external_helper = function("external_helper", vec![call("foreign")]);
        let dynamic_helper = function("dynamic_helper", vec![call("callback")]);
        let main_owner = crate::core::resolved::NodeId("function:main".into());
        let external_owner =
            crate::core::resolved::nested_function_owner(&main_owner, &external_helper);
        let dynamic_owner =
            crate::core::resolved::nested_function_owner(&main_owner, &dynamic_helper);
        let main = function(
            "main",
            vec![
                Stmt::Func(external_helper),
                Stmt::Func(dynamic_helper),
                call("external_helper"),
                call("dynamic_helper"),
            ],
        );

        let analysis = infer_items(&[Item::ExternBlock(external), Item::Func(main)]);
        assert!(analysis.summary("main").unwrap().inferred.is_unknown());
        assert!(analysis
            .owner_summaries
            .get(&external_owner.0)
            .expect("external helper summary")
            .inferred
            .is_unknown());
        assert!(analysis
            .owner_summaries
            .get(&dynamic_owner.0)
            .expect("dynamic helper summary")
            .inferred
            .is_unknown());
    }

    #[test]
    fn real_io_network_blocking_and_suspend_facets_are_preserved() {
        let source = vec![Item::Func(function(
            "runtime_facets",
            vec![
                Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident("read_file".into())),
                    vec![],
                )),
                Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident("write_file".into())),
                    vec![],
                )),
                Stmt::Expr(Expr::Call(Box::new(Expr::Ident("fetch".into())), vec![])),
                Stmt::Expr(Expr::Call(Box::new(Expr::Ident("sleep_ms".into())), vec![])),
                Stmt::Expr(Expr::Await(Box::new(Expr::Ident("future".into())))),
                Stmt::Expr(Expr::Spawn(Box::new(Expr::Ident("work".into())))),
            ],
        ))];
        let summary = infer_items(&source)
            .summary("runtime_facets")
            .unwrap()
            .clone();
        assert!(summary.inferred.contains(EffectKind::FileRead));
        assert!(summary.inferred.contains(EffectKind::FileWrite));
        assert!(summary.inferred.contains(EffectKind::Network));
        assert!(summary.inferred.contains(EffectKind::Blocking));
        assert!(summary.inferred.contains(EffectKind::Suspend));
        assert!(summary.inferred.contains(EffectKind::Spawn));
    }

    #[test]
    fn guarded_and_opaque_builtin_sinks_do_not_fall_through_as_pure() {
        // These names are all registered language builtins.  The checker
        // scanner has a compatibility branch for known pure builtins, so a
        // missing entry here would silently erase a real sink from the
        // callable summary and let the canonical route look pure.
        assert!(builtin_effects("read_file_guarded").contains(EffectKind::FileRead));
        assert!(builtin_effects("write_file_guarded").contains(EffectKind::FileWrite));
        let guarded_http = builtin_effects("http_get_guarded");
        assert!(guarded_http.contains(EffectKind::Network));
        assert!(guarded_http.contains(EffectKind::Blocking));
        assert!(builtin_effects("bind").contains(EffectKind::Network));
        assert!(builtin_effects("get_env_guarded").is_unknown());
        assert!(builtin_effects("exit").is_unknown());

        let names = [
            "read_file_guarded",
            "write_file_guarded",
            "http_get_guarded",
            "get_env_guarded",
            "exit",
        ];
        let body = names
            .into_iter()
            .map(|name| {
                Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident(name.to_string())),
                    Vec::new(),
                ))
            })
            .collect();
        let summary = infer_items(&[Item::Func(function("sinks", body))])
            .summary("sinks")
            .expect("sink summary")
            .clone();
        assert!(summary.inferred.contains(EffectKind::FileRead));
        assert!(summary.inferred.contains(EffectKind::FileWrite));
        assert!(summary.inferred.contains(EffectKind::Network));
        assert!(summary.inferred.contains(EffectKind::Blocking));
        assert!(summary.inferred.is_unknown());
    }
}
