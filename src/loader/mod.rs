// Loader module — module resolution and file loading.
// Uses Flow-based module loader (v0.29.4).

mod flow;

pub use self::flow::{flow_load_file, flow_load_main, Acc};

use crate::ast::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Public types ────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct LoadedModule {
    pub path: PathBuf,
    pub file: File,
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
}

impl ModuleLoader {
    pub fn new(base_dir: PathBuf) -> Self {
        ModuleLoader {
            base_dir,
            loaded: HashMap::new(),
            modules: HashMap::new(),
        }
    }

    pub fn load_main(&mut self, path: &Path) -> Result<LoadedModule, String> {
        let mut acc = Acc::new(self.base_dir.clone());
        acc.loaded = self.loaded.clone();
        acc.modules = self.modules.clone();
        let (acc, main) = flow_load_main(acc, path)?;
        self.loaded = acc.loaded;
        self.modules = acc.modules;
        Ok(main)
    }

    pub fn merge_all(&self) -> Result<File, String> {
        flow::flow_merge_all(&self.modules)
    }
}

// ── Prelude loading ─────────────────────────────────────────────────────

pub fn load_prelude_items() -> Vec<Item> {
    let std_dir = match stdlib_dir() {
        Some(d) => d,
        None => return vec![],
    };
    let prelude_path = std_dir.join("prelude.mimi");
    if !prelude_path.exists() {
        return vec![];
    }
    let source = match std::fs::read_to_string(&prelude_path) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let tokens = match crate::lexer::Lexer::new(&source).tokenize() {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    match crate::parser::Parser::new(tokens).parse_file() {
        Ok(file) => file.items,
        Err(_) => vec![],
    }
}

pub fn merge_prelude_into(dest: &mut File) {
    let prelude_items = load_prelude_items();
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

// ── Legacy loader for test equivalence ──────────────────────────────────

#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod legacy {
    use super::*;
    use crate::lexer;
    use crate::lockfile;
    use crate::manifest;
    use crate::parser;
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
                            let dep_path = dir.join(path_str);
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
            let source = std::fs::read_to_string(path)
                .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
            let tokens = lexer::Lexer::new(&source)
                .tokenize()
                .map_err(|e| format!("lexer error in {}: {}", path.display(), e))?;
            let file = parser::Parser::new(tokens)
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
