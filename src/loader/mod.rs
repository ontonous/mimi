// Loader module — module resolution and file loading.
// Uses Flow-based module loader (v0.29.4).

mod flow;

pub use self::flow::{flow_load_file, flow_load_main, Acc};

use crate::ast::*;
use crate::span::{SourceContext, SourceId, SourceRegistry, SourceTextOrigin};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Public types ────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct LoadedModule {
    pub path: PathBuf,
    pub file: File,
}

/// A loader failure with the most precise source anchor available and the
/// registry needed to route that anchor.  String-returning public APIs remain
/// available for compatibility; source-aware tools should use the diagnostic
/// entry point on [`ModuleLoader`].
#[derive(Clone, Debug)]
pub struct LoadDiagnosticError {
    pub diagnostic: Box<crate::diagnostic::Diagnostic>,
    pub sources: SourceRegistry,
}

impl LoadDiagnosticError {
    pub(crate) fn global(message: impl Into<String>, sources: SourceRegistry) -> Self {
        Self {
            diagnostic: Box::new(crate::diagnostic::Diagnostic::error(
                message,
                crate::span::Span::UNKNOWN,
            )),
            sources,
        }
    }

    pub(crate) fn at(
        message: impl Into<String>,
        span: crate::span::Span,
        sources: SourceRegistry,
    ) -> Self {
        Self {
            diagnostic: Box::new(crate::diagnostic::Diagnostic::error(message, span)),
            sources,
        }
    }
}

impl std::fmt::Display for LoadDiagnosticError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.diagnostic.message)
    }
}

impl std::error::Error for LoadDiagnosticError {}

/// Register a disk entry point using the exact workspace/package/stdlib
/// identity rules used by the transitive module loader.
pub fn source_context_for_path(path: &Path) -> Result<SourceContext, String> {
    let base_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let (acc, source_id) = Acc::new(base_dir).source_id_for(path, SourceTextOrigin::Disk)?;
    let (_, registry) = acc.into_source_registry();
    SourceContext::registered(source_id, registry).map_err(|error| error.to_string())
}

/// Create a production parser for a disk source without duplicating loader
/// identity logic at individual CLI/tool entry points.
pub fn parser_for_path(
    tokens: Vec<crate::lexer::Token>,
    path: &Path,
) -> Result<crate::parser::Parser, String> {
    Ok(crate::parser::Parser::new_with_source_context(
        tokens,
        source_context_for_path(path)?,
    ))
}

/// Sketch-mode counterpart of [`parser_for_path`].
pub fn sketch_parser_for_path(
    tokens: Vec<crate::lexer::Token>,
    path: &Path,
) -> Result<crate::parser::Parser, String> {
    Ok(crate::parser::Parser::new_sketch_with_source_context(
        tokens,
        source_context_for_path(path)?,
    ))
}

// ── stdlib_dir (pure) ──────────────────────────────────────────────────

/// Get the path to the built-in standard library directory.
pub fn stdlib_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MIMI_STDLIB") {
        // B1: validate MIMI_STDLIB to prevent code injection via path traversal.
        // The stdlib directory must be an absolute path (set by the system/user
        // for legitimate override purposes), but we reject NUL bytes and
        // check it exists.
        if !dir.contains('\0') {
            let p = PathBuf::from(dir);
            if p.is_absolute() && p.exists() {
                return Some(p);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let dev = exe_dir.join("std");
            if dev.exists() {
                return Some(dev);
            }
            let installed = exe_dir
                .parent()
                .map(|p| p.join("lib").join("mimi").join("std"));
            if let Some(ref installed) = installed {
                if installed.exists() {
                    return Some(installed.clone());
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir: Option<&Path> = Some(&cwd);
        while let Some(d) = dir {
            let candidate = d.join("std");
            if candidate.exists() && candidate.is_dir() {
                return Some(candidate);
            }
            dir = d.parent();
        }
    }
    let compile_time = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std");
    if compile_time.exists() {
        return Some(compile_time);
    }
    None
}

// ── ModuleLoader (delegates to Flow) ────────────────────────────────────

pub struct ModuleLoader {
    pub base_dir: PathBuf,
    pub loaded: HashMap<PathBuf, LoadedModule>,
    pub modules: HashMap<String, LoadedModule>,
    source_ids: HashMap<PathBuf, SourceId>,
    source_registry: SourceRegistry,
}

impl ModuleLoader {
    pub fn new(base_dir: PathBuf) -> Self {
        ModuleLoader {
            base_dir,
            loaded: HashMap::new(),
            modules: HashMap::new(),
            source_ids: HashMap::new(),
            source_registry: SourceRegistry::default(),
        }
    }

    pub fn load_main(&mut self, path: &Path) -> Result<LoadedModule, String> {
        let mut acc = Acc::new(self.base_dir.clone())
            .with_source_registry(self.source_ids.clone(), self.source_registry.clone());
        acc.loaded = self.loaded.clone();
        acc.modules = self.modules.clone();
        let (acc, main) = flow_load_main(acc, path)?;
        self.loaded = acc.loaded.clone();
        self.modules = acc.modules.clone();
        (self.source_ids, self.source_registry) = acc.into_source_registry();
        Ok(main)
    }

    /// L-C1: load main using an already-parsed in-memory `File` so unsaved
    /// editor buffers are not overwritten by stale on-disk content.
    pub fn load_main_with_file(
        &mut self,
        path: &Path,
        file: crate::ast::File,
    ) -> Result<LoadedModule, String> {
        let mut acc = Acc::new(self.base_dir.clone())
            .with_source_registry(self.source_ids.clone(), self.source_registry.clone());
        acc.loaded = self.loaded.clone();
        acc.modules = self.modules.clone();
        let (acc, main) = flow::flow_load_main_with_file(acc, path, file)?;
        self.loaded = acc.loaded.clone();
        self.modules = acc.modules.clone();
        (self.source_ids, self.source_registry) = acc.into_source_registry();
        Ok(main)
    }

    /// Source-aware counterpart of [`Self::load_main_with_file`].  On failure
    /// it retains dependency source records and an exact import/lexer/parser
    /// span whenever one exists.
    pub fn load_main_with_file_diagnostic(
        &mut self,
        path: &Path,
        file: crate::ast::File,
    ) -> Result<LoadedModule, LoadDiagnosticError> {
        let mut acc = Acc::new(self.base_dir.clone())
            .with_source_registry(self.source_ids.clone(), self.source_registry.clone());
        acc.loaded = self.loaded.clone();
        acc.modules = self.modules.clone();
        match flow::flow_load_main_with_file_diagnostic(acc, path, file) {
            Ok((acc, main)) => {
                self.loaded = acc.loaded.clone();
                self.modules = acc.modules.clone();
                (self.source_ids, self.source_registry) = acc.into_source_registry();
                Ok(main)
            }
            Err(error) => {
                self.source_ids = error
                    .sources
                    .records()
                    .iter()
                    .filter_map(|record| {
                        record
                            .disk_path
                            .as_ref()
                            .map(|path| (path.clone(), record.id))
                    })
                    .collect();
                self.source_registry = error.sources.clone();
                Err(error)
            }
        }
    }

    pub fn source_registry(&self) -> &SourceRegistry {
        &self.source_registry
    }

    pub fn merge_all(&self) -> Result<File, String> {
        flow::flow_merge_all(&self.modules)
    }
}

// ── Prelude loading ─────────────────────────────────────────────────────

/// Load prelude items. CL-H18: log errors at each step instead of silently
/// returning an empty vec, so missing/broken prelude files are diagnosable.
pub fn load_prelude_items() -> Vec<Item> {
    let mut source_registry = SourceRegistry::default();
    load_prelude_items_with_registry(&mut source_registry)
}

fn load_prelude_items_with_registry(source_registry: &mut SourceRegistry) -> Vec<Item> {
    let std_dir = match stdlib_dir() {
        Some(d) => d,
        None => {
            eprintln!(
                "[mimi] warning: stdlib directory not found; prelude not loaded \
                 (set MIMI_STDLIB or run from a checkout that contains std/)"
            );
            return vec![];
        }
    };
    let prelude_path = std_dir.join("prelude.mimi");
    if !prelude_path.exists() {
        eprintln!(
            "[mimi] warning: prelude file missing: {}",
            prelude_path.display()
        );
        return vec![];
    }
    // CL-H1: size-cap prelude reads the same way as CLI source loads.
    let source = match crate::path_safety::read_source_capped(&prelude_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[mimi] warning: failed to read prelude: {}", e);
            return vec![];
        }
    };
    let tokens = match crate::lexer::Lexer::new(&source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[mimi] warning: failed to tokenize prelude: {}", e);
            return vec![];
        }
    };
    let prelude_path = prelude_path.canonicalize().unwrap_or(prelude_path);
    let source_record = crate::span::SourceRecord::new(
        crate::span::SourceKey::new("stdlib:prelude.mimi").expect("prelude SourceKey is non-empty"),
        crate::span::SourceTextOrigin::Builtin,
    )
    .with_uri(format!(
        "file://{}",
        prelude_path.to_string_lossy().replace('\\', "/")
    ))
    .with_disk_path(prelude_path);
    let source_id = match source_registry.register(source_record) {
        Ok(source_id) => source_id,
        Err(error) => {
            eprintln!("[mimi] warning: failed to register prelude source: {error}");
            return vec![];
        }
    };
    match crate::parser::Parser::new_with_source_registry(
        tokens,
        source_id,
        source_registry.clone(),
    )
    .parse_file()
    {
        Ok(file) => file.items,
        Err(e) => {
            eprintln!("[mimi] warning: failed to parse prelude: {}", e);
            vec![]
        }
    }
}

pub fn merge_prelude_into(dest: &mut File) {
    let prelude_items = load_prelude_items_with_registry(&mut dest.sources);
    if prelude_items.is_empty() {
        return;
    }
    let existing: std::collections::HashSet<String> = dest
        .items
        .iter()
        .filter_map(|i| item_name(i).map(String::from))
        .collect();
    let mut new_items: Vec<Item> = Vec::new();
    for item in prelude_items {
        if let Some(name) = item_name(&item) {
            if !existing.contains(name) {
                new_items.push(item);
            }
        } else {
            new_items.push(item);
        }
    }
    dest.items.splice(0..0, new_items);
}

fn item_name(item: &Item) -> Option<&str> {
    match item {
        Item::Func(f) => Some(&f.name),
        Item::Module(m) => Some(&m.name),
        Item::Type(t) => Some(&t.name),
        Item::Actor(a) => Some(&a.name),
        Item::Cap(c) => Some(&c.name),
        Item::Trait(t) => Some(&t.name),
        Item::Impl(i) => Some(i.type_name.as_str()),
        Item::ExternBlock(_) => None,
        Item::Const { name, .. } => Some(name),
        Item::Flow(f) => Some(&f.name),
        Item::Protocol(p) => Some(&p.name),
        Item::Session(s) => Some(&s.name),
    }
}

#[cfg(test)]
mod source_entry_tests {
    use super::{load_prelude_items_with_registry, parser_for_path, source_context_for_path};
    use crate::ast::Item;
    use crate::span::SourceRegistry;

    #[test]
    fn disk_entry_parser_uses_loader_key_and_routes_parse_errors() {
        let root = std::env::temp_dir().join(format!("mimi_source_entry_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source_dir = root.join("src");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        std::fs::write(
            root.join("mimi.toml"),
            "[package]\nname = \"source-entry-test\"\nversion = \"0.1.0\"\n",
        )
        .expect("write manifest");
        let path = source_dir.join("main.mimi");
        let source = "func broken(value: i32 -> i32 { value }";
        std::fs::write(&path, source).expect("write source");

        let context = source_context_for_path(&path).expect("register disk source");
        let key = context
            .registry()
            .key(context.source_id())
            .expect("source key")
            .as_str();
        assert_eq!(key, "workspace:src/main.mimi");
        assert!(!key.contains(root.to_string_lossy().as_ref()));

        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let error = parser_for_path(tokens, &path)
            .expect("disk parser")
            .parse_file()
            .expect_err("malformed signature");
        assert!(error.source_id.is_known());
        assert_eq!(error.to_diagnostic().span.source_id, error.source_id);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parsed_prelude_ast_metadata_uses_the_stable_stdlib_source() {
        let mut sources = SourceRegistry::default();
        let items = load_prelude_items_with_registry(&mut sources);
        let identity = items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "identity" => Some(function),
                _ => None,
            })
            .expect("stdlib prelude should contain identity");

        let source_id = identity.meta.span.source_id;
        assert!(source_id.is_known());
        assert_eq!(
            sources.key(source_id).map(|key| key.as_str()),
            Some("stdlib:prelude.mimi")
        );
        assert_eq!(identity.params[0].meta.span.source_id, source_id);
        assert_eq!(identity.generics[0].meta.span.source_id, source_id);
        assert_eq!(
            identity.body[0]
                .meta()
                .expect("parsed prelude statement metadata")
                .span
                .source_id,
            source_id
        );
    }
}

// ── Legacy loader for test equivalence ──────────────────────────────────

#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod legacy {
    use super::*;
    use crate::lexer;
    use crate::lockfile;
    use crate::manifest;
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    #[derive(Clone, Debug)]
    pub struct LegacyLoader {
        pub base_dir: PathBuf,
        pub loaded: HashMap<PathBuf, LoadedModule>,
        pub modules: HashMap<String, LoadedModule>,
        dep_paths: HashMap<String, PathBuf>,
        lock_entries: HashMap<String, lockfile::LockEntry>,
        visiting: HashSet<PathBuf>,
    }

    impl LegacyLoader {
        fn module_key(&self, path: &Path) -> String {
            path.strip_prefix(&self.base_dir)
                .or_else(|_| path.strip_prefix(std::env::current_dir().unwrap_or_default()))
                .unwrap_or(path)
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/")
        }

        pub fn new(base_dir: PathBuf) -> Self {
            let mut loader = Self {
                base_dir: base_dir.clone(),
                loaded: HashMap::new(),
                modules: HashMap::new(),
                dep_paths: HashMap::new(),
                lock_entries: HashMap::new(),
                visiting: HashSet::new(),
            };
            if let Ok(Some((dir, m))) = manifest::Manifest::find(&base_dir) {
                if let Some(deps) = &m.dependencies {
                    for dep in deps {
                        if let Some(path_str) = &dep.path {
                            // Path deps may use ../sibling (monorepo). Absolute/NUL rejected.
                            let dep_path =
                                match crate::path_safety::resolve_path_dep(&dir, path_str) {
                                    Ok(p) => p,
                                    Err(_) => continue,
                                };
                            if dep_path.exists() {
                                loader.dep_paths.insert(dep.name.clone(), dep_path);
                            }
                        }
                    }
                }
                if let Ok(Some(lf)) = lockfile::Lockfile::load(&dir) {
                    for entry in lf.package {
                        loader.lock_entries.insert(entry.name.clone(), entry);
                    }
                }
            }
            loader
        }

        fn load_file(&mut self, path: &Path) -> Result<LoadedModule, String> {
            if let Some(m) = self.loaded.get(path) {
                return Ok(LoadedModule {
                    path: m.path.clone(),
                    file: m.file.clone(),
                });
            }
            if !self.visiting.insert(path.to_path_buf()) {
                return Err(format!("circular dependency: {}", path.display()));
            }
            let result = self.load_file_inner(path);
            self.visiting.remove(path); // CL-C3: always clean up, even on error
            result
        }

        fn load_file_inner(&mut self, path: &Path) -> Result<LoadedModule, String> {
            // CL-H1: reject oversized module sources before parse.
            let source = crate::path_safety::read_source_capped(path)?;
            let tokens = lexer::Lexer::new(&source)
                .tokenize()
                .map_err(|e| format!("lexer error in {}: {}", path.display(), e))?;
            let file = super::parser_for_path(tokens, path)?
                .parse_file()
                .map_err(|e| format!("parse error in {}: {}", path.display(), e))?;

            let module_name = self.module_key(path);
            let loaded = LoadedModule {
                path: path.to_path_buf(),
                file,
            };

            let imports = loaded.file.imports.clone();
            for import in &imports {
                let import_path = self.resolve_import(path, &import.path)?;
                let dep = self.load_file(&import_path)?;
                let dep_name = self.module_key(&import_path);
                self.modules.insert(dep_name, dep);
            }

            self.modules.insert(module_name, loaded.clone());
            self.loaded.insert(path.to_path_buf(), loaded.clone());
            Ok(loaded)
        }

        fn resolve_import(&self, from: &Path, path: &[String]) -> Result<PathBuf, String> {
            for segment in path {
                if segment == ".." || segment.contains('/') || segment.contains('\\') {
                    return Err(format!(
                        "import path '{}' contains invalid segment '{}'",
                        path.join("::"),
                        segment
                    ));
                }
            }
            let base = from.parent().unwrap_or(&self.base_dir);
            let relative: PathBuf = path.iter().collect();

            let file_path = base.join(&relative).with_extension("mimi");
            if file_path.exists() {
                return Ok(file_path);
            }

            let base_path = self.base_dir.join(&relative).with_extension("mimi");
            if base_path.exists() {
                return Ok(base_path);
            }

            if let Some(first) = path.first() {
                if let Some(dep_dir) = self.dep_paths.get(first) {
                    let dep_relative: PathBuf = path.iter().skip(1).collect();
                    let dep_path = dep_dir.join(&dep_relative).with_extension("mimi");
                    if dep_path.exists() {
                        return Ok(dep_path);
                    }
                    let dep_root = dep_dir.with_extension("mimi");
                    if dep_root.exists() && path.len() == 1 {
                        return Ok(dep_root);
                    }
                    if path.len() == 1 && dep_dir.is_dir() {
                        if let Ok(Some((manifest_dir, manifest))) =
                            manifest::Manifest::find(dep_dir)
                        {
                            let entry_path = manifest.entry_path(&manifest_dir);
                            if entry_path.exists() {
                                return Ok(entry_path);
                            }
                        }
                    }
                }
            }

            let deps_dir = self.base_dir.join(".mimi").join("deps");
            if deps_dir.exists() {
                if let Some(first) = path.first() {
                    let dep_root = deps_dir.join(first);
                    if dep_root.exists() {
                        let dep_relative: PathBuf = path.iter().skip(1).collect();
                        let dep_path = dep_root.join(&dep_relative).with_extension("mimi");
                        if dep_path.exists() {
                            return Ok(dep_path);
                        }
                        let dep_root_file = dep_root.with_extension("mimi");
                        if dep_root_file.exists() && path.len() == 1 {
                            return Ok(dep_root_file);
                        }
                    }
                }
            }

            if path.first().map(|s| s == "std").unwrap_or(false) {
                if let Some(std_dir) = super::stdlib_dir() {
                    let std_path = std_dir.join(&relative).with_extension("mimi");
                    if std_path.exists() {
                        return Ok(std_path);
                    }
                    let sub_path: PathBuf = path.iter().skip(1).collect();
                    let std_path2 = std_dir.join(&sub_path).with_extension("mimi");
                    if std_path2.exists() {
                        return Ok(std_path2);
                    }
                }
            }

            if let Some(std_dir) = super::stdlib_dir() {
                let std_path = std_dir.join(&relative).with_extension("mimi");
                if std_path.exists() {
                    return Ok(std_path);
                }
            }

            if path.len() >= 2 {
                let prefix: PathBuf = path[..path.len() - 1].iter().collect();
                let prefix_file = base.join(&prefix).with_extension("mimi");
                if prefix_file.exists() {
                    return Ok(prefix_file);
                }
                let base_prefix = self.base_dir.join(&prefix).with_extension("mimi");
                if base_prefix.exists() {
                    return Ok(base_prefix);
                }
                if let Some(first) = path.first() {
                    if let Some(dep_dir) = self.dep_paths.get(first) {
                        let dep_prefix: PathBuf = path[1..path.len() - 1].iter().collect();
                        let dep_path = dep_dir.join(&dep_prefix).with_extension("mimi");
                        if dep_path.exists() {
                            return Ok(dep_path);
                        }
                        if dep_prefix.as_os_str().is_empty() && dep_dir.is_dir() {
                            if let Ok(Some((manifest_dir, m))) = manifest::Manifest::find(dep_dir) {
                                let entry_path = m.entry_path(&manifest_dir);
                                if entry_path.exists() {
                                    return Ok(entry_path);
                                }
                            }
                        }
                    }
                }
                if let Some(std_dir) = super::stdlib_dir() {
                    if path.first().map(|s| s == "std").unwrap_or(false) {
                        let sub_prefix: PathBuf = path[1..path.len() - 1].iter().collect();
                        let std_prefix = std_dir.join(&sub_prefix).with_extension("mimi");
                        if std_prefix.exists() {
                            return Ok(std_prefix);
                        }
                    }
                    let std_prefix = std_dir.join(&prefix).with_extension("mimi");
                    if std_prefix.exists() {
                        return Ok(std_prefix);
                    }
                }
            }

            Err(format!(
                "cannot find module '{}' (looked in {}, {}, and stdlib)",
                path.join("::"),
                base.display(),
                self.base_dir.display()
            ))
        }

        pub fn load_main(&mut self, path: &Path) -> Result<LoadedModule, String> {
            let canonical = path
                .canonicalize()
                .map_err(|e| format!("cannot resolve path {}: {}", path.display(), e))?;
            self.load_file(&canonical)
        }

        pub fn merge_all(&self) -> Result<File, String> {
            let mut all_items = Vec::new();
            let mut seen_imports = HashSet::new();
            let mut all_imports = Vec::new();
            let mut seen_names: HashSet<String> = HashSet::new();

            for module in self.modules.values() {
                for item in &module.file.items {
                    if let Some(name) = item_name(item) {
                        if !seen_names.insert(name.to_string()) {
                            let dup_modules: Vec<String> = self
                                .modules
                                .values()
                                .filter(|m| m.file.items.iter().any(|i| item_name(i) == Some(name)))
                                .map(|m| m.path.display().to_string())
                                .collect();
                            return Err(format!(
                                "duplicate item '{}' found in modules: {}",
                                name,
                                dup_modules.join(", ")
                            ));
                        }
                    }
                }
                all_items.extend(module.file.items.clone());
                for imp in &module.file.imports {
                    if seen_imports.insert(imp.path.clone()) {
                        all_imports.push(imp.clone());
                    }
                }
            }

            Ok(File {
                sources: crate::span::SourceRegistry::default(),
                imports: all_imports,
                items: all_items,
                implicit_single: false,
            })
        }
    }

    fn item_name(item: &Item) -> Option<&str> {
        match item {
            Item::Func(f) => Some(&f.name),
            Item::Module(m) => Some(&m.name),
            Item::Type(t) => Some(&t.name),
            Item::Actor(a) => Some(&a.name),
            Item::Cap(c) => Some(&c.name),
            Item::Trait(t) => Some(&t.name),
            Item::Impl(i) => Some(i.type_name.as_str()),
            Item::ExternBlock(_) => None,
            Item::Const { name, .. } => Some(name),
            Item::Flow(f) => Some(&f.name),
            Item::Protocol(p) => Some(&p.name),
            Item::Session(s) => Some(&s.name),
        }
    }
}

#[cfg(test)]
mod source_registry_tests {
    use super::ModuleLoader;
    use std::fs;

    #[test]
    fn persistent_module_loader_keeps_source_identity_across_cache_hits() {
        let dir = std::env::temp_dir().join(format!(
            "mimi_module_loader_source_cache_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("test directory");
        let main_path = dir.join("main.mimi");
        let lib_path = dir.join("lib.mimi");
        fs::write(&main_path, "use lib;\nfunc main() -> i32 { lib::value() }")
            .expect("main source");
        fs::write(&lib_path, "pub func value() -> i32 { 7 }").expect("lib source");

        let mut loader = ModuleLoader::new(dir.clone());
        loader.load_main(&main_path).expect("first load");
        let canonical_main = main_path.canonicalize().expect("canonical main");
        let canonical_lib = lib_path.canonicalize().expect("canonical lib");
        let main_id = loader
            .source_registry()
            .id_for_disk_path(&canonical_main)
            .expect("main id");
        let lib_id = loader
            .source_registry()
            .id_for_disk_path(&canonical_lib)
            .expect("lib id");
        let main_key = loader
            .source_registry()
            .key(main_id)
            .cloned()
            .expect("main key");
        let lib_key = loader
            .source_registry()
            .key(lib_id)
            .cloned()
            .expect("lib key");

        loader.load_main(&main_path).expect("cached main load");
        loader
            .load_main(&lib_path)
            .expect("cached dependency as main");
        assert_eq!(
            loader.source_registry().id_for_disk_path(&canonical_main),
            Some(main_id)
        );
        assert_eq!(
            loader.source_registry().id_for_disk_path(&canonical_lib),
            Some(lib_id)
        );
        assert_eq!(loader.source_registry().len(), 2);
        assert_eq!(loader.source_registry().key(main_id), Some(&main_key));
        assert_eq!(loader.source_registry().key(lib_id), Some(&lib_key));
        assert_ne!(main_key, lib_key);

        let merged = loader.merge_all().expect("merge cached modules");
        assert_eq!(merged.sources.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }
}
