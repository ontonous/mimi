use crate::ast::*;
use crate::{core, lexer, parser, manifest, lockfile};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Get the path to the built-in standard library directory.
/// Resolved relative to the mimi binary (../std/) or overridable via MIMI_STDLIB env var.
fn stdlib_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MIMI_STDLIB") {
        let p = PathBuf::from(dir);
        if p.exists() { return Some(p); }
    }
    // Resolve relative to the binary: target/debug/mimi -> mimi/std/
    // or installed as /usr/bin/mimi -> /usr/lib/mimi/std/
    if let Ok(exe) = std::env::current_exe() {
        // Try: <exe_dir>/../std/  (developer layout)
        if let Some(exe_dir) = exe.parent() {
            let dev = exe_dir.join("std");
            if dev.exists() { return Some(dev); }
            // Try: <exe_dir>/../lib/mimi/std/  (installed layout)
            let installed = exe_dir.parent()
                .map(|p| p.join("lib").join("mimi").join("std"));
            if let Some(ref installed) = installed {
                if installed.exists() { return Some(installed.clone()); }
            }
        }
    }
    // Fallback: relative to current directory's parent (project root during development)
    if let Ok(cwd) = std::env::current_dir() {
        let fallback = cwd.join("std");
        if fallback.exists() { return Some(fallback); }
        // Check one level up (running from mimi/tests/)
        let parent = cwd.parent().map(|p| p.join("std"));
        if let Some(ref p) = parent { if p.exists() { return Some(p.clone()); } }
    }
    None
}

/// Loaded module with its parsed AST and file path
#[derive(Clone, Debug)]
pub struct LoadedModule {
    pub path: PathBuf,
    pub file: File,
}

/// Module loader: resolves use paths and loads .mimi files
pub struct ModuleLoader {
    /// Base directory for resolving relative paths
    base_dir: PathBuf,
    /// Cache of loaded modules by path
    loaded: HashMap<PathBuf, LoadedModule>,
    /// Cache of loaded modules by module name
    modules: HashMap<String, LoadedModule>,
    /// Dependency paths from mimi.toml: dep_name -> resolved path
    dep_paths: HashMap<String, PathBuf>,
    /// Lockfile entries keyed by dependency name
    lock_entries: HashMap<String, lockfile::LockEntry>,
    /// Set of paths currently being loaded (cycle detection)
    visiting: HashSet<PathBuf>,
}

impl ModuleLoader {
    /// Compute a module key from a path: relative path from base_dir with / separators,
    /// avoiding collisions between files with the same stem in different directories.
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
        // Try to load mimi.toml and resolve dependency paths
        if let Ok(Some((dir, manifest))) = manifest::Manifest::find(&base_dir) {
            if let Some(deps) = &manifest.dependencies {
                for dep in deps {
                    if let Some(path_str) = &dep.path {
                        let dep_path = dir.join(path_str);
                        if dep_path.exists() {
                            loader.dep_paths.insert(dep.name.clone(), dep_path);
                        }
                    }
                }
            }
            // Also load the lockfile if present, for reproducibility verification
            if let Ok(Some(lf)) = lockfile::Lockfile::load(&dir) {
                for entry in lf.package {
                    loader.lock_entries.insert(entry.name.clone(), entry);
                }
            }
        }
        loader
    }

    /// Load the main file and all its transitive imports
    pub fn load_main(&mut self, path: &Path) -> Result<LoadedModule, String> {
        let canonical = path.canonicalize()
            .map_err(|e| format!("cannot resolve path {}: {}", path.display(), e))?;
        self.load_file(&canonical)
    }

    /// Load a file and resolve its imports recursively
    fn load_file(&mut self, path: &Path) -> Result<LoadedModule, String> {
        // Check cache
        if let Some(m) = self.loaded.get(path) {
            return Ok(LoadedModule {
                path: m.path.clone(),
                file: m.file.clone(),
            });
        }

        // Cycle detection
        if !self.visiting.insert(path.to_path_buf()) {
            return Err(format!("circular dependency detected: {} imports itself", path.display()));
        }

        // Read and parse
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        let tokens = lexer::Lexer::new(&source).tokenize()
            .map_err(|e| format!("lexer error in {}: {}", path.display(), e))?;
        let file = parser::Parser::new(tokens).parse_file()
            .map_err(|e| format!("parse error in {}: {}", path.display(), e))?;

        let module_name = self.module_key(path);

        let loaded = LoadedModule {
            path: path.to_path_buf(),
            file,
        };

        // Resolve imports
        let imports = loaded.file.imports.clone();
        for import in &imports {
            let import_path = self.resolve_import(path, &import.path)?;
            let dep = self.load_file(&import_path)?;
            let dep_name = self.module_key(&import_path);
            self.modules.insert(dep_name, dep);
        }

        self.visiting.remove(path);

        self.modules.insert(module_name, loaded.clone());
        self.loaded.insert(path.to_path_buf(), loaded.clone());

        Ok(loaded)
    }

    /// Resolve a use path to a file path
    fn resolve_import(&self, from: &Path, path: &[String]) -> Result<PathBuf, String> {
        for segment in path {
            if segment == ".." || segment.contains('/') || segment.contains('\\') {
                return Err(format!(
                    "import path '{}' contains invalid segment '{}'",
                    path.join("::"), segment
                ));
            }
        }
        let base = from.parent().unwrap_or(&self.base_dir);

        // Simple resolution: join the path segments and add .mimi extension
        let relative: PathBuf = path.iter().collect();
        let file_path = base.join(&relative).with_extension("mimi");

        if file_path.exists() {
            return Ok(file_path);
        }

        // Try relative to base_dir
        let base_path = self.base_dir.join(&relative).with_extension("mimi");
        if base_path.exists() {
            return Ok(base_path);
        }

        // Try dependency paths from mimi.toml
        if let Some(first) = path.first() {
            if let Some(dep_dir) = self.dep_paths.get(first) {
                let dep_relative: PathBuf = path.iter().skip(1).collect();
                let dep_path = dep_dir.join(&dep_relative).with_extension("mimi");
                if dep_path.exists() {
                    return Ok(dep_path);
                }
                // Try with the dep_dir itself as the module root
                let dep_root = dep_dir.with_extension("mimi");
                if dep_root.exists() && path.len() == 1 {
                    return Ok(dep_root);
                }
            }
        }

        // Try .mimi/deps/ (dependencies installed via `mimi install`)
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

        // Try built-in stdlib (import "std/io.mimi" or @import "std/io.mimi")
        if path.first().map(|s| s == "std").unwrap_or(false) {
            if let Some(std_dir) = stdlib_dir() {
                let std_path = std_dir.join(&relative).with_extension("mimi");
                if std_path.exists() {
                    return Ok(std_path);
                }
                // Also try without the "std" prefix (since std_dir IS std/)
                let sub_path: PathBuf = path.iter().skip(1).collect();
                let std_path2 = std_dir.join(&sub_path).with_extension("mimi");
                if std_path2.exists() {
                    return Ok(std_path2);
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

    /// Get a loaded module by name
    #[allow(dead_code)]
    pub fn get_module(&self, name: &str) -> Option<&LoadedModule> {
        self.modules.get(name)
    }

    /// Merge all loaded module items into a single file for interpretation.
    /// Detects duplicate item names and emits a diagnostic (returns Err) if found.
    pub fn merge_all(&self) -> Result<File, String> {
        let mut all_items = Vec::new();
        let mut seen_imports = std::collections::HashSet::new();
        let mut all_imports = Vec::new();
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        for module in self.modules.values() {
            for item in &module.file.items {
                if let Some(name) = item_name(item) {
                    if !seen_names.insert(name.to_string()) {
                        let dup_modules: Vec<String> = self.modules.values()
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
        })
    }

    /// Type-check all loaded modules
    #[allow(dead_code)]
    pub fn check_all(&self) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        for module in self.modules.values() {
            core::check(&module.file)?;
        }
        Ok(())
    }
}

/// Extract the name from an Item for duplicate detection in merge_all.
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
    }
}
