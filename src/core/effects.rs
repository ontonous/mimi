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
    key: String,
    function: &'a FuncDef,
    public: bool,
}

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
    let mut callables = Vec::<Callable<'_>>::new();
    let mut by_short = HashMap::<String, Vec<String>>::new();
    let mut extern_names = HashSet::new();
    for item in items {
        match item {
            Item::Func(function) => {
                add_callable(
                    &mut callables,
                    &mut by_short,
                    function.name.clone(),
                    function,
                );
            }
            Item::Impl(implementation) => {
                for function in &implementation.methods {
                    let key = format!("{}::{}", implementation.type_name, function.name);
                    add_callable(&mut callables, &mut by_short, key, function);
                }
            }
            Item::Actor(actor) => {
                for function in &actor.methods {
                    let key = format!("{}::{}", actor.name, function.name);
                    add_callable(&mut callables, &mut by_short, key, function);
                }
            }
            Item::ExternBlock(block) => {
                extern_names.extend(block.funcs.iter().map(|function| function.name.clone()));
            }
            _ => {}
        }
    }

    let mut direct = BTreeMap::<String, DirectEffects>::new();
    for callable in &callables {
        let mut effects = DirectEffects::default();
        scan_block(
            &callable.function.body,
            &mut effects,
            &by_short,
            &extern_names,
        );
        direct.insert(callable.key.clone(), effects);
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
            let mut next = inferred.get(&callable.key).cloned().unwrap_or_default();
            let Some(effects) = direct.get(&callable.key) else {
                continue;
            };
            for callee in &effects.callees {
                if let Some(targets) = by_short.get(callee) {
                    for target in targets {
                        if let Some(target_effects) = inferred.get(target) {
                            changed |= next.union_with(target_effects);
                        }
                    }
                } else {
                    next.insert(EffectKind::Unknown);
                    changed = true;
                }
            }
            changed |= inferred
                .insert(callable.key.clone(), next.clone())
                .is_some_and(|old| old != next);
        }
        if !changed {
            break;
        }
    }

    let mut analysis = EffectAnalysis::default();
    for callable in callables {
        let inferred_set = inferred.remove(&callable.key).unwrap_or_default();
        let declared = if callable.function.effects.is_empty() {
            None
        } else {
            match EffectSet::from_names(callable.function.effects.iter().map(String::as_str)) {
                Ok(set) => Some(set),
                Err(error) => {
                    analysis.diagnostics.push(EffectDiagnostic {
                        callable: callable.key.clone(),
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
                .get(&callable.key)
                .map(|effects| effects.sources.clone())
                .unwrap_or_default(),
        };
        // A declaration is an upper bound, not a way to hide an effect.  Keep
        // the inferred set authoritative and report the mismatch once.
        if let Some(declared) = &summary.declared {
            if summary.inferred.is_unknown() {
                analysis.diagnostics.push(EffectDiagnostic {
                    callable: callable.key.clone(),
                    message: format!(
                        "callable '{}' reaches an unknown effect and cannot satisfy declared upper bound {}",
                        callable.key, declared
                    ),
                });
            } else if !summary.inferred.is_subset_of(declared) {
                analysis.diagnostics.push(EffectDiagnostic {
                    callable: callable.key.clone(),
                    message: format!(
                        "callable '{}' inferred effects {} exceed declared upper bound {}",
                        callable.key, summary.inferred, declared
                    ),
                });
            }
        }
        // Public declarations with an explicit effect list are validated by
        // default.  Existing stdlib/public code with no declaration remains
        // compatible until the new surface declaration is selected; callers
        // can use `validate_public_summaries` for a strict module boundary.
        if callable.public && callable.function.effects.len() > 0 {
            if let Err(message) = summary.validate_public(&callable.key) {
                analysis.diagnostics.push(EffectDiagnostic {
                    callable: callable.key.clone(),
                    message,
                });
            }
        }
        analysis.summaries.insert(callable.key, summary);
    }
    analysis
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
    by_short: &mut HashMap<String, Vec<String>>,
    key: String,
    function: &'a FuncDef,
) {
    by_short
        .entry(function.name.clone())
        .or_default()
        .push(key.clone());
    callables.push(Callable {
        key,
        function,
        public: function.pub_,
    });
}

fn scan_block(
    block: &Block,
    effects: &mut DirectEffects,
    by_short: &HashMap<String, Vec<String>>,
    extern_names: &HashSet<String>,
) {
    for statement in block {
        scan_stmt(statement, effects, by_short, extern_names);
    }
}

fn scan_stmt(
    statement: &Stmt,
    effects: &mut DirectEffects,
    by_short: &HashMap<String, Vec<String>>,
    extern_names: &HashSet<String>,
) {
    match statement.unlocated() {
        Stmt::Located { .. } => unreachable!(),
        Stmt::Let { init, .. } => init
            .as_ref()
            .into_iter()
            .for_each(|expr| scan_expr(expr, effects, by_short, extern_names)),
        Stmt::Return(value) | Stmt::Break(value) => value
            .as_ref()
            .into_iter()
            .for_each(|expr| scan_expr(expr, effects, by_short, extern_names)),
        Stmt::Expr(expr)
        | Stmt::Drop(expr)
        | Stmt::Requires(expr, _)
        | Stmt::Ensures(expr, _)
        | Stmt::Invariant(expr, _) => scan_expr(expr, effects, by_short, extern_names),
        Stmt::If { cond, then_, else_ } => {
            scan_expr(cond, effects, by_short, extern_names);
            scan_block(then_, effects, by_short, extern_names);
            if let Some(other) = else_ {
                scan_block(other, effects, by_short, extern_names);
            }
        }
        Stmt::IfLet {
            init, then_, else_, ..
        } => {
            scan_expr(init, effects, by_short, extern_names);
            scan_block(then_, effects, by_short, extern_names);
            if let Some(other) = else_ {
                scan_block(other, effects, by_short, extern_names);
            }
        }
        Stmt::While { cond, body } => {
            scan_expr(cond, effects, by_short, extern_names);
            scan_block(body, effects, by_short, extern_names);
        }
        Stmt::WhileLet { init, body, .. } => {
            scan_expr(init, effects, by_short, extern_names);
            scan_block(body, effects, by_short, extern_names);
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
            scan_block(body, effects, by_short, extern_names);
        }
        Stmt::For { iterable, body, .. } => {
            scan_expr(iterable, effects, by_short, extern_names);
            scan_block(body, effects, by_short, extern_names);
        }
        Stmt::Assign { target, value } => {
            scan_expr(target, effects, by_short, extern_names);
            scan_expr(value, effects, by_short, extern_names);
        }
        Stmt::SharedLet { init, .. } => {
            effects.add(EffectKind::SharedMutation, "shared binding");
            scan_expr(init, effects, by_short, extern_names);
        }
        Stmt::Pinned { expr, body, .. } => {
            effects.add(EffectKind::Unknown, "pinned/FFI boundary");
            scan_expr(expr, effects, by_short, extern_names);
            scan_block(body, effects, by_short, extern_names);
        }
        Stmt::Func(function) => scan_block(&function.body, effects, by_short, extern_names),
        Stmt::Continue | Stmt::Math(_) | Stmt::Ellipsis => {}
    }
}

fn scan_expr(
    expression: &Expr,
    effects: &mut DirectEffects,
    by_short: &HashMap<String, Vec<String>>,
    extern_names: &HashSet<String>,
) {
    match expression.unlocated() {
        Expr::Located { .. } => unreachable!(),
        Expr::Call(callee, arguments) => {
            match callee.unlocated() {
                Expr::Ident(name) => {
                    let builtin_facets = builtin_effects(name);
                    if !builtin_facets.is_empty() {
                        for effect in builtin_facets.iter() {
                            effects.add(effect, format!("builtin '{name}'"));
                        }
                    } else if crate::core::builtins::is_builtin_callable(name)
                        || crate::core::builtins::is_language_intrinsic_callable(name)
                        || crate::core::builtins::is_language_constructor(name)
                    {
                        // Known pure builtin/intrinsic.  Being absent from
                        // the effect table is an intentional pure result;
                        // it must not be confused with a dynamic call.
                    } else if extern_names.contains(name) {
                        effects.add(EffectKind::Unknown, format!("extern '{name}'"));
                    } else if by_short.contains_key(name) {
                        effects.callees.insert(name.clone());
                    } else {
                        // Calls through a closure/generic function parameter or
                        // an unresolved dynamic value are not pure.
                        effects.add(EffectKind::Unknown, format!("dynamic call '{name}'"));
                    }
                }
                Expr::Field(_, method) => {
                    if by_short.contains_key(method) {
                        effects.callees.insert(method.clone());
                    } else if is_pure_builtin_method(method) {
                        // Option/Result observation and extraction methods do
                        // not perform an effect.  They may still trap on an
                        // invalid value; traps are represented by the
                        // canonical runtime error channel, not by `unknown`.
                    } else {
                        effects.add(EffectKind::Unknown, format!("dynamic method '{method}'"));
                    }
                }
                _ => {
                    effects.add(EffectKind::Unknown, "dynamic call");
                }
            }
            scan_expr(callee, effects, by_short, extern_names);
            for argument in arguments {
                scan_expr(argument, effects, by_short, extern_names);
            }
        }
        Expr::Spawn(inner) => {
            effects.add(EffectKind::Spawn, "spawn expression");
            scan_expr(inner, effects, by_short, extern_names);
        }
        Expr::Await(inner) => {
            effects.add(EffectKind::Suspend, "await expression");
            scan_expr(inner, effects, by_short, extern_names);
        }
        Expr::Lambda { body, .. } => scan_block(body, effects, by_short, extern_names),
        Expr::If { cond, then_, else_ } => {
            scan_expr(cond, effects, by_short, extern_names);
            scan_block(then_, effects, by_short, extern_names);
            if let Some(other) = else_ {
                scan_block(other, effects, by_short, extern_names);
            }
        }
        Expr::Block(body) | Expr::Arena(body) | Expr::Quote(body) | Expr::Comptime(body) => {
            scan_block(body, effects, by_short, extern_names)
        }
        Expr::Binary(_, left, right) => {
            scan_expr(left, effects, by_short, extern_names);
            scan_expr(right, effects, by_short, extern_names);
        }
        Expr::Unary(_, inner)
        | Expr::Try(inner)
        | Expr::OptionalChain(inner, _)
        | Expr::TypeOf(inner)
        | Expr::Old(inner)
        | Expr::QuoteInterpolate(inner) => scan_expr(inner, effects, by_short, extern_names),
        Expr::Field(base, _) | Expr::TupleIndex(base, _) => {
            scan_expr(base, effects, by_short, extern_names)
        }
        Expr::Index(base, index) => {
            scan_expr(base, effects, by_short, extern_names);
            scan_expr(index, effects, by_short, extern_names);
        }
        Expr::Tuple(values) | Expr::List(values) | Expr::SetLiteral(values) => {
            for value in values {
                scan_expr(value, effects, by_short, extern_names);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            scan_expr(expr, effects, by_short, extern_names);
            scan_expr(iter, effects, by_short, extern_names);
            if let Some(guard) = guard {
                scan_expr(guard, effects, by_short, extern_names);
            }
        }
        Expr::Match(subject, arms) => {
            scan_expr(subject, effects, by_short, extern_names);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    scan_expr(guard, effects, by_short, extern_names);
                }
                scan_expr(&arm.body, effects, by_short, extern_names);
            }
        }
        Expr::Record { fields, rest, .. } => {
            for field in fields {
                scan_expr(&field.value, effects, by_short, extern_names);
            }
            if let Some(rest) = rest {
                scan_expr(rest, effects, by_short, extern_names);
            }
        }
        Expr::SliceExpr { target, start, end } => {
            scan_expr(target, effects, by_short, extern_names);
            if let Some(start) = start {
                scan_expr(start, effects, by_short, extern_names);
            }
            if let Some(end) = end {
                scan_expr(end, effects, by_short, extern_names);
            }
        }
        Expr::Turbofish(name, _, arguments) => {
            if by_short.contains_key(name) {
                effects.callees.insert(name.clone());
            } else {
                effects.add(
                    EffectKind::Unknown,
                    format!("dynamic generic call '{name}'"),
                );
            }
            for argument in arguments {
                scan_expr(argument, effects, by_short, extern_names);
            }
        }
        Expr::NamedArg(_, inner) | Expr::Cast(inner, _) => {
            scan_expr(inner, effects, by_short, extern_names)
        }
        Expr::Literal(Lit::FString(parts)) => {
            for part in parts {
                if let crate::ast::FStringPart::Interp(expr) = part {
                    scan_expr(&expr, effects, by_short, extern_names)
                }
            }
        }
        Expr::MapLiteral { entries } => {
            for (key, value) in entries {
                scan_expr(key, effects, by_short, extern_names);
                scan_expr(value, effects, by_short, extern_names);
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
    use crate::ast::{AstNodeMeta, AstOrigin, Block, FuncDef, Item};

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
