//! Stable module identities, public interface summaries, and conservative
//! incremental invalidation facts.
//!
//! The production loader still exposes the historical file-merge view to the
//! checker and to old callers.  This module is the identity layer that sits
//! beside that view: it derives a deterministic package/module graph from the
//! loaded files without changing the AST or silently changing merge semantics.
//! Consumers that need qualified lookup can use the [`ModuleGraph`] facts;
//! consumers that have not migrated continue using [`ModuleLoader::merge_all`].

use crate::ast::{AstNodeMeta, Item, TypeDefKind};
use crate::loader::{LoadedModule, ModuleLoader};
use crate::manifest;
use crate::span::{SourceKey, Span};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

/// Stable identity of a source module.  `package`, `version`, and `path` are
/// all part of the identity; a bare file name is deliberately insufficient.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ModuleId {
    pub package: String,
    pub version: Option<String>,
    /// Package-relative path without the `.mimi` suffix, using `/` separators.
    pub path: String,
}

impl ModuleId {
    pub fn new(
        package: impl Into<String>,
        version: Option<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            package: package.into(),
            version,
            path: path.into(),
        }
    }

    pub fn stable_text(&self) -> String {
        let path = self.path.replace('/', "::");
        match &self.version {
            Some(version) if !version.is_empty() => {
                format!("{}@{}::{}", self.package, version, path)
            }
            _ => format!("{}::{}", self.package, path),
        }
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.stable_text())
    }
}

/// Convert a loader `SourceKey` into the import path spelling used for
/// qualified access. This is a best-effort helper for checker/tooling layers;
/// unresolved/external memory keys return `None` and callers must fail closed.
pub fn module_path_from_source_key(key: &SourceKey) -> Option<String> {
    let value = key.as_str();
    let (namespace, rest) = value.split_once(':')?;
    match namespace {
        "stdlib" => Some(format!(
            "std::{}",
            without_mimi_suffix(rest).replace('/', "::")
        )),
        "workspace" => Some(without_mimi_suffix(rest).replace('/', "::")),
        "package" => {
            let (package, relative) = rest.split_once(':')?;
            let package = package.split('@').next().unwrap_or(package);
            Some(format!(
                "{}::{}",
                package,
                without_mimi_suffix(relative).replace('/', "::")
            ))
        }
        _ => None,
    }
}

/// Return whether a source identity can satisfy an import path from the
/// importing module.
///
/// `SourceKey` identities for workspace files are rooted at the workspace
/// manifest (for example `workspace:apps/demo/helper.mimi`), while imports
/// are resolved relative to the importing file (`use helper` or
/// `use apps::demo::helper`).  Comparing the two spellings byte-for-byte
/// therefore loses valid aliases as soon as the entry point is below the
/// workspace root.  Keep identity matching conservative: accept an exact
/// path, a path-suffix match for relative workspace imports, and a package
/// root import (`use package`) whose source identity starts with that package
/// name.  Do not reduce either side to a basename; sibling modules with the
/// same file name must remain distinct.
pub fn module_path_matches_import_path(module_path: &str, import_path: &str) -> bool {
    let module = module_path
        .trim_matches(':')
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let import = import_path
        .trim_matches(':')
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if module.is_empty() || import.is_empty() {
        return false;
    }
    if module == import {
        return true;
    }
    // Relative imports can omit the workspace-root prefix.  Requiring at
    // least two path components avoids turning a short import such as
    // `use helper` into a basename search across every nested `helper.mimi`.
    // Root-level `use helper` still matches the exact module identity above.
    if import.len() > 1
        && module.len() > import.len()
        && module[module.len() - import.len()..] == import[..]
    {
        return true;
    }
    // A package-root import resolves to the package entry module.  Its source
    // identity carries the package name plus the entry path (often `main`),
    // so retain the package segment as an explicit, narrow exception.
    import.len() == 1 && module.first() == import.first()
}

/// Stable identity for an exported or imported declaration.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SymbolId {
    pub module: ModuleId,
    pub name: String,
}

impl SymbolId {
    pub fn stable_text(&self) -> String {
        format!("{}::{}", self.module, self.name)
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.stable_text())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SymbolKind {
    Function,
    Type,
    Actor,
    Capability,
    Trait,
    Impl,
    Constant,
    Flow,
    Session,
    Extern,
}

impl SymbolKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Type => "type",
            Self::Actor => "actor",
            Self::Capability => "capability",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Constant => "constant",
            Self::Flow => "flow",
            Self::Session => "session",
            Self::Extern => "extern",
        }
    }
}

/// One public declaration in a module interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceSymbol {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub signature: String,
    pub public: bool,
    pub effects: Vec<String>,
    pub abi: Option<String>,
    /// Internal body identity for a Mimi implementation with an exported ABI.
    /// It is intentionally distinct from `wrapper_symbol`.
    pub body_symbol: Option<String>,
    /// External ABI wrapper identity, when `abi` is present.
    pub wrapper_symbol: Option<String>,
    pub span: Span,
}

impl InterfaceSymbol {
    fn sort_key(&self) -> (String, SymbolKind, String) {
        (self.id.name.clone(), self.kind, self.signature.clone())
    }

    pub fn canonical_text(&self) -> String {
        let effects = self.effects.join(",");
        format!(
            "{}|{}|{}|public={}|effects=[{}]|abi={}|body={}|wrapper={}",
            self.id.stable_text(),
            self.kind.as_str(),
            self.signature,
            self.public,
            effects,
            self.abi.as_deref().unwrap_or(""),
            self.body_symbol.as_deref().unwrap_or(""),
            self.wrapper_symbol.as_deref().unwrap_or("")
        )
    }
}

/// Deterministic public interface summary.  `unknown` is set when a source
/// boundary (dynamic/FFI/generated code) prevents a complete static summary;
/// it is never silently treated as a pure or zero-cost interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleInterface {
    pub module: ModuleId,
    pub exports: Vec<InterfaceSymbol>,
    pub hash: String,
    pub unknown: bool,
}

impl ModuleInterface {
    pub fn symbol(&self, name: &str) -> Option<&InterfaceSymbol> {
        self.exports.iter().find(|symbol| symbol.id.name == name)
    }

    pub fn canonical_text(&self) -> String {
        self.exports
            .iter()
            .map(InterfaceSymbol::canonical_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn diff(&self, previous: &Self) -> InterfaceDiff {
        let old = previous
            .exports
            .iter()
            .map(|symbol| (symbol.id.clone(), symbol.canonical_text()))
            .collect::<BTreeMap<_, _>>();
        let new = self
            .exports
            .iter()
            .map(|symbol| (symbol.id.clone(), symbol.canonical_text()))
            .collect::<BTreeMap<_, _>>();
        let added = new
            .keys()
            .filter(|key| !old.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        let removed = old
            .keys()
            .filter(|key| !new.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        let changed = new
            .iter()
            .filter_map(|(key, value)| {
                old.get(key)
                    .filter(|old_value| *old_value != value)
                    .map(|_| key.clone())
            })
            .collect::<Vec<_>>();
        InterfaceDiff {
            added,
            removed,
            changed,
            unknown: self.unknown || previous.unknown,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceDiff {
    pub added: Vec<SymbolId>,
    pub removed: Vec<SymbolId>,
    pub changed: Vec<SymbolId>,
    pub unknown: bool,
}

impl InterfaceDiff {
    pub fn is_changed(&self) -> bool {
        self.unknown
            || !self.added.is_empty()
            || !self.removed.is_empty()
            || !self.changed.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleEdge {
    pub from: ModuleId,
    pub to: ModuleId,
    pub alias: String,
    pub path: Vec<String>,
    /// True when the edge could not be made statically precise (for example a
    /// generated/dynamic import). Such edges conservatively invalidate users.
    pub unknown: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleNode {
    pub id: ModuleId,
    pub path: PathBuf,
    pub source_key: Option<SourceKey>,
    pub interface: ModuleInterface,
    pub body_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleGraph {
    pub root: Option<ModuleId>,
    pub modules: BTreeMap<ModuleId, ModuleNode>,
    pub edges: Vec<ModuleEdge>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleGraphError {
    MissingImport { module: ModuleId, path: String },
    Cycle(Vec<ModuleId>),
}

impl fmt::Display for ModuleGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingImport { module, path } => {
                write!(
                    formatter,
                    "module '{}' imports unresolved module '{}'",
                    module, path
                )
            }
            Self::Cycle(path) => write!(
                formatter,
                "circular module dependency: {}",
                path.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
        }
    }
}

impl std::error::Error for ModuleGraphError {}

impl ModuleGraph {
    /// Construct an identity graph from a production `ModuleLoader` snapshot.
    /// The historical merge view remains untouched. Missing imports and cycles
    /// are errors rather than guesses, keeping incremental invalidation
    /// fail-closed.
    pub fn from_loader(loader: &ModuleLoader) -> Result<Self, ModuleGraphError> {
        let mut modules = BTreeMap::new();
        let mut by_path = HashMap::new();
        let mut root = None;
        for loaded in loader.loaded.values() {
            let id = module_id_for_path(&loader.base_dir, &loaded.path);
            if root.is_none() && is_entry_path(&loader.base_dir, &loaded.path) {
                root = Some(id.clone());
            }
            by_path.insert(normalize_path(&loaded.path), id.clone());
            let interface = module_interface(&id, loaded);
            let body_hash = module_body_hash(loaded);
            let source_key = loaded
                .file
                .items
                .iter()
                .find_map(item_meta)
                .and_then(|meta| loaded.file.sources.key(meta.span.source_id).cloned());
            modules.insert(
                id.clone(),
                ModuleNode {
                    id,
                    path: normalize_path(&loaded.path),
                    source_key,
                    interface,
                    body_hash,
                },
            );
        }

        let mut edges = Vec::new();
        for loaded in loader.loaded.values() {
            let from = by_path
                .get(&normalize_path(&loaded.path))
                .cloned()
                .expect("loaded module must be in graph");
            for import in &loaded.file.imports {
                let target_path = resolve_loaded_import(&loaded.path, &import.path, loader);
                let Some(target_path) = target_path else {
                    return Err(ModuleGraphError::MissingImport {
                        module: from.clone(),
                        path: import.path.join("::"),
                    });
                };
                let Some(to) = by_path.get(&normalize_path(&target_path)).cloned() else {
                    return Err(ModuleGraphError::MissingImport {
                        module: from.clone(),
                        path: import.path.join("::"),
                    });
                };
                edges.push(ModuleEdge {
                    from: from.clone(),
                    to,
                    alias: import
                        .alias
                        .clone()
                        .or_else(|| import.path.last().cloned())
                        .unwrap_or_default(),
                    path: import.path.clone(),
                    unknown: false,
                });
            }
        }
        edges.sort_by(|a, b| {
            a.from
                .cmp(&b.from)
                .then(a.to.cmp(&b.to))
                .then(a.alias.cmp(&b.alias))
                .then(a.path.cmp(&b.path))
        });
        if let Some(cycle) = detect_cycle(&modules, &edges) {
            return Err(ModuleGraphError::Cycle(cycle));
        }
        Ok(Self {
            root,
            modules,
            edges,
        })
    }

    pub fn interface(&self, module: &ModuleId) -> Option<&ModuleInterface> {
        self.modules.get(module).map(|node| &node.interface)
    }

    pub fn dependents_of(&self, module: &ModuleId) -> Vec<ModuleId> {
        let mut seen = BTreeSet::new();
        let mut queue = vec![module.clone()];
        while let Some(current) = queue.pop() {
            for edge in self.edges.iter().filter(|edge| edge.to == current) {
                if seen.insert(edge.from.clone()) {
                    queue.push(edge.from.clone());
                }
            }
        }
        seen.into_iter().collect()
    }

    pub fn canonical_text(&self) -> String {
        let mut lines = Vec::new();
        for node in self.modules.values() {
            lines.push(format!("module {} hash={}", node.id, node.interface.hash));
            for symbol in &node.interface.exports {
                lines.push(format!("  {}", symbol.canonical_text()));
            }
        }
        for edge in &self.edges {
            lines.push(format!(
                "edge {} -> {} as {}",
                edge.from, edge.to, edge.alias
            ));
        }
        lines.join("\n")
    }

    /// Compare two graph snapshots.  Interface changes invalidate all static
    /// dependents. Private body changes are reported separately and only
    /// invalidate the changed module; callers may widen this set for dynamic or
    /// FFI edges marked `unknown`.
    pub fn invalidation(&self, previous: &Self) -> InvalidationReport {
        let all_ids = self
            .modules
            .keys()
            .chain(previous.modules.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut changed_modules = Vec::new();
        let mut api_changed = Vec::new();
        let mut implementation_changed = Vec::new();
        let mut invalidated = BTreeSet::new();
        let mut unknown = false;
        for id in all_ids {
            match (previous.modules.get(&id), self.modules.get(&id)) {
                (None, Some(_)) | (Some(_), None) => {
                    changed_modules.push(id.clone());
                    api_changed.push(id.clone());
                    invalidated.insert(id);
                }
                (Some(old), Some(new)) => {
                    if old.interface.hash != new.interface.hash
                        || old.interface.unknown != new.interface.unknown
                    {
                        changed_modules.push(id.clone());
                        api_changed.push(id.clone());
                        invalidated.insert(id.clone());
                    } else if old.body_hash != new.body_hash {
                        changed_modules.push(id.clone());
                        implementation_changed.push(id.clone());
                        invalidated.insert(id.clone());
                    }
                }
                (None, None) => unreachable!(),
            }
        }
        for id in api_changed.iter().chain(implementation_changed.iter()) {
            for dependent in self.dependents_of(id) {
                // A private body change does not force a caller recompile. An
                // interface change does; keep the two sets distinguishable.
                if api_changed.contains(id) {
                    invalidated.insert(dependent);
                }
            }
        }
        for edge in self.edges.iter().chain(previous.edges.iter()) {
            if edge.unknown {
                unknown = true;
                invalidated.extend(self.modules.keys().cloned());
            }
        }
        changed_modules.sort();
        api_changed.sort();
        implementation_changed.sort();
        InvalidationReport {
            changed_modules,
            api_changed,
            implementation_changed,
            invalidated: invalidated.into_iter().collect(),
            unknown,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidationReport {
    pub changed_modules: Vec<ModuleId>,
    pub api_changed: Vec<ModuleId>,
    pub implementation_changed: Vec<ModuleId>,
    pub invalidated: Vec<ModuleId>,
    pub unknown: bool,
}

fn module_interface(id: &ModuleId, loaded: &LoadedModule) -> ModuleInterface {
    let mut exports = Vec::new();
    let mut unknown = false;
    for item in &loaded.file.items {
        let Some((name, kind, public, signature, effects, abi, body, wrapper, meta)) =
            item_symbol(item)
        else {
            continue;
        };
        if !public {
            continue;
        }
        if matches!(kind, SymbolKind::Extern) {
            unknown = true;
        }
        exports.push(InterfaceSymbol {
            id: SymbolId {
                module: id.clone(),
                name,
            },
            kind,
            signature,
            public,
            effects,
            abi,
            body_symbol: body,
            wrapper_symbol: wrapper,
            span: meta.span,
        });
    }
    exports.sort_by_key(|a| a.sort_key());
    let canonical = exports
        .iter()
        .map(InterfaceSymbol::canonical_text)
        .collect::<Vec<_>>()
        .join("\n");
    ModuleInterface {
        module: id.clone(),
        exports,
        hash: format!("{:016x}", stable_hash(canonical.as_bytes())),
        unknown,
    }
}

fn item_symbol(
    item: &Item,
) -> Option<(
    String,
    SymbolKind,
    bool,
    String,
    Vec<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    AstNodeMeta,
)> {
    let fmt_type = crate::core::fmt_type;
    match item {
        Item::Func(function) => {
            let signature = format!(
                "({}) -> {}",
                function
                    .params
                    .iter()
                    .map(|param| format!("{}: {}", param.name, fmt_type(&param.ty)))
                    .collect::<Vec<_>>()
                    .join(", "),
                function
                    .ret
                    .as_ref()
                    .map(fmt_type)
                    .unwrap_or_else(|| "unit".to_string())
            );
            let body = function.extern_abi.as_ref().map(|_| {
                format!(
                    "mimi_body_{:016x}",
                    stable_hash(format!("{}:{:?}", function.name, function.body).as_bytes())
                )
            });
            let wrapper = function
                .extern_abi
                .as_ref()
                .map(|abi| format!("{}:{}", abi, function.name));
            Some((
                function.name.clone(),
                SymbolKind::Function,
                function.pub_ || function.extern_abi.is_some(),
                signature,
                function.effects.clone(),
                function.extern_abi.clone(),
                body,
                wrapper,
                function.meta,
            ))
        }
        Item::Type(definition) => Some((
            definition.name.clone(),
            SymbolKind::Type,
            definition.pub_,
            type_signature(definition),
            Vec::new(),
            None,
            None,
            None,
            definition.meta,
        )),
        Item::Actor(actor) => Some((
            actor.name.clone(),
            SymbolKind::Actor,
            actor.pub_,
            format!("actor {}", actor.name),
            Vec::new(),
            None,
            None,
            None,
            actor.meta,
        )),
        Item::Cap(cap) => Some((
            cap.name.clone(),
            SymbolKind::Capability,
            true,
            format!("cap {}", cap.name),
            Vec::new(),
            None,
            None,
            None,
            cap.meta,
        )),
        Item::Trait(trait_def) => Some((
            trait_def.name.clone(),
            SymbolKind::Trait,
            true,
            format!("trait {}", trait_def.name),
            Vec::new(),
            None,
            None,
            None,
            trait_def.meta,
        )),
        Item::Impl(impl_def) => Some((
            format!("{}::{}", impl_def.trait_name, impl_def.type_name),
            SymbolKind::Impl,
            true,
            format!("impl {} for {}", impl_def.trait_name, impl_def.type_name),
            Vec::new(),
            None,
            None,
            None,
            impl_def.meta,
        )),
        Item::Const {
            meta,
            name,
            ty,
            pub_,
            ..
        } => Some((
            name.clone(),
            SymbolKind::Constant,
            *pub_,
            format!(
                "const {}: {}",
                name,
                ty.as_ref().map(fmt_type).unwrap_or_else(|| "_".into())
            ),
            Vec::new(),
            None,
            None,
            None,
            *meta,
        )),
        Item::Flow(flow) => Some((
            flow.name.clone(),
            SymbolKind::Flow,
            flow.pub_,
            format!("flow {}", flow.name),
            Vec::new(),
            None,
            None,
            None,
            flow.meta,
        )),
        Item::Session(session) => Some((
            session.name.clone(),
            SymbolKind::Session,
            session.pub_,
            format!("session {}", session.name),
            Vec::new(),
            None,
            None,
            None,
            session.meta,
        )),
        Item::ExternBlock(block) => Some((
            format!("extern:{}", block.abi),
            SymbolKind::Extern,
            true,
            format!("extern {} ({})", block.abi, block.funcs.len()),
            Vec::new(),
            Some(block.abi.clone()),
            None,
            None,
            block.meta,
        )),
    }
}

fn type_signature(definition: &crate::ast::TypeDef) -> String {
    match &definition.kind {
        TypeDefKind::Alias(target) => format!(
            "type {} = {}",
            definition.name,
            crate::core::fmt_type(target)
        ),
        TypeDefKind::Newtype(target) => format!(
            "newtype {} = {}",
            definition.name,
            crate::core::fmt_type(target)
        ),
        TypeDefKind::Record(fields) | TypeDefKind::Union(fields) => format!(
            "type {} {{ {} }}",
            definition.name,
            fields
                .iter()
                .map(|field| format!("{}: {}", field.name, crate::core::fmt_type(&field.ty)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeDefKind::Enum(variants) => format!(
            "type {} = {}",
            definition.name,
            variants
                .iter()
                .map(|variant| variant.name.clone())
                .collect::<Vec<_>>()
                .join(" | ")
        ),
    }
}

fn item_meta(item: &Item) -> Option<&AstNodeMeta> {
    Some(match item {
        Item::Func(value) => &value.meta,
        Item::Type(value) => &value.meta,
        Item::Actor(value) => &value.meta,
        Item::Cap(value) => &value.meta,
        Item::Trait(value) => &value.meta,
        Item::Impl(value) => &value.meta,
        Item::ExternBlock(value) => &value.meta,
        Item::Const { meta, .. } => meta,
        Item::Flow(value) => &value.meta,
        Item::Session(value) => &value.meta,
    })
}

fn module_body_hash(loaded: &LoadedModule) -> String {
    let text = loaded
        .file
        .items
        .iter()
        .map(|item| format!("{:?}", item))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{:016x}", stable_hash(text.as_bytes()))
}

fn module_id_for_path(base_dir: &Path, path: &Path) -> ModuleId {
    let normalized = normalize_path(path);
    if let Some(std_root) = crate::loader::stdlib_dir().map(|root| normalize_path(&root)) {
        if let Ok(relative) = normalized.strip_prefix(std_root) {
            return ModuleId::new("std", Some("builtin".into()), without_extension(relative));
        }
    }
    if let Ok(Some((manifest_dir, manifest))) = manifest::Manifest::find(&normalized) {
        let package = manifest
            .package
            .as_ref()
            .map(|package| package.name.clone())
            .unwrap_or_else(|| "workspace".into());
        let version = manifest
            .package
            .as_ref()
            .and_then(|package| package.version.clone());
        let path = normalized
            .strip_prefix(normalize_path(&manifest_dir))
            .map(without_extension)
            .unwrap_or_else(|_| without_extension(&normalized));
        return ModuleId::new(package, version, path);
    }
    let path = normalized
        .strip_prefix(normalize_path(base_dir))
        .map(without_extension)
        .unwrap_or_else(|_| without_extension(&normalized));
    ModuleId::new("workspace", None, path)
}

fn is_entry_path(base_dir: &Path, path: &Path) -> bool {
    manifest::Manifest::find(path)
        .ok()
        .flatten()
        .is_some_and(|(root, manifest)| {
            normalize_path(&manifest.entry_path(&root)) == normalize_path(path)
        })
        || normalize_path(&base_dir.join("main.mimi")) == normalize_path(path)
}

fn resolve_loaded_import(from: &Path, path: &[String], loader: &ModuleLoader) -> Option<PathBuf> {
    let relative: PathBuf = path.iter().collect();
    let candidates = [
        from.parent()
            .unwrap_or(&loader.base_dir)
            .join(&relative)
            .with_extension("mimi"),
        loader.base_dir.join(&relative).with_extension("mimi"),
    ];
    for candidate in candidates {
        if loader.loaded.contains_key(&normalize_path(&candidate)) {
            return Some(candidate);
        }
    }
    loader
        .loaded
        .keys()
        .find(|candidate| {
            let id = module_id_for_path(&loader.base_dir, candidate);
            let wanted = path.join("::");
            id.path == wanted
                || id.path.ends_with(&format!("/{}", wanted))
                || (path.first().map(String::as_str) == Some("std")
                    && id.path == path[1..].join("::"))
        })
        .cloned()
}

fn detect_cycle(
    modules: &BTreeMap<ModuleId, ModuleNode>,
    edges: &[ModuleEdge],
) -> Option<Vec<ModuleId>> {
    let mut outgoing: BTreeMap<ModuleId, Vec<ModuleId>> = BTreeMap::new();
    for edge in edges {
        outgoing
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }
    fn visit(
        node: &ModuleId,
        outgoing: &BTreeMap<ModuleId, Vec<ModuleId>>,
        active: &mut Vec<ModuleId>,
        seen: &mut BTreeSet<ModuleId>,
    ) -> Option<Vec<ModuleId>> {
        if let Some(index) = active.iter().position(|item| item == node) {
            let mut cycle = active[index..].to_vec();
            cycle.push(node.clone());
            return Some(cycle);
        }
        if !seen.insert(node.clone()) {
            return None;
        }
        active.push(node.clone());
        for child in outgoing.get(node).into_iter().flatten() {
            if let Some(cycle) = visit(child, outgoing, active, seen) {
                return Some(cycle);
            }
        }
        active.pop();
        None
    }
    let mut active = Vec::new();
    let mut seen = BTreeSet::new();
    for node in modules.keys() {
        if let Some(cycle) = visit(node, &outgoing, &mut active, &mut seen) {
            return Some(cycle);
        }
    }
    None
}

fn without_extension(path: &Path) -> String {
    path.with_extension("")
        .to_string_lossy()
        .trim_end_matches('.')
        .replace('\\', "/")
        .trim_start_matches('/')
        .replace('/', "::")
}

fn without_mimi_suffix(path: &str) -> String {
    path.strip_suffix(".mimi")
        .unwrap_or(path)
        .trim_matches('/')
        .to_string()
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_module_import_matching_is_path_specific() {
        assert!(module_path_matches_import_path(
            "target::apps::demo::helper",
            "apps::demo::helper"
        ));
        assert!(!module_path_matches_import_path(
            "target::apps::demo::helper",
            "helper"
        ));
        assert!(!module_path_matches_import_path(
            "target::apps::other::helper",
            "apps::demo::helper"
        ));
        assert!(!module_path_matches_import_path(
            "target::apps::other::helper",
            "apps::demo::parse"
        ));
    }

    #[test]
    fn module_and_symbol_ids_include_package_version_and_path() {
        let module = ModuleId::new("parse", Some("1.2.0".into()), "src/core");
        let symbol = SymbolId {
            module: module.clone(),
            name: "parse".into(),
        };
        assert_eq!(module.to_string(), "parse@1.2.0::src::core");
        assert_eq!(symbol.to_string(), "parse@1.2.0::src::core::parse");
    }

    #[test]
    fn interface_diff_distinguishes_added_removed_and_changed_symbols() {
        let module = ModuleId::new("p", None, "main");
        let symbol = |name: &str, signature: &str| InterfaceSymbol {
            id: SymbolId {
                module: module.clone(),
                name: name.into(),
            },
            kind: SymbolKind::Function,
            signature: signature.into(),
            public: true,
            effects: Vec::new(),
            abi: None,
            body_symbol: None,
            wrapper_symbol: None,
            span: Span::UNKNOWN,
        };
        let old = ModuleInterface {
            module: module.clone(),
            exports: vec![symbol("parse", "() -> i32")],
            hash: "old".into(),
            unknown: false,
        };
        let new = ModuleInterface {
            module: module.clone(),
            exports: vec![symbol("parse", "() -> string"), symbol("scan", "() -> i32")],
            hash: "new".into(),
            unknown: false,
        };
        let diff = new.diff(&old);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.removed.len(), 0);
        assert_eq!(diff.changed.len(), 1);
        assert!(diff.is_changed());
    }
}
