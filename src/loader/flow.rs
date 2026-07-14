//! Flow-based Mimi module loader — iterative worklist approach.
//!
//! Replaces `&mut self` recursion with a flat BFS worklist:
//! no state machine enum, no `&mut self`, just explicit state threading.
//!
//! Entry points:
//!   - `flow_load_main(acc, path) -> (acc, LoadedModule)` — load main file + all transitive deps
//!   - `flow_load_file(acc, path) -> (acc, LoadedModule)` — load a single file (cached)

use crate::ast::{File, Item};

type ResolvedImport = (Vec<String>, PathBuf);
type ImportEdge = (PathBuf, Vec<ResolvedImport>);
use crate::lexer;
use crate::lockfile;
use crate::manifest;
use crate::parser;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ── Import public types from parent module ─────────────────────────────

use super::LoadedModule;

// ── Accumulator (replaces &mut self on ModuleLoader) ────────────────────

#[derive(Clone, Default)]
pub struct Acc {
    pub loaded: HashMap<PathBuf, LoadedModule>,
    pub modules: HashMap<String, LoadedModule>,
    pub dep_paths: HashMap<String, PathBuf>,
    pub lock_entries: HashMap<String, lockfile::LockEntry>,
    pub base_dir: PathBuf,
}

impl Acc {
    pub fn new(base_dir: PathBuf) -> Self {
        let mut acc = Acc {
            base_dir,
            ..Default::default()
        };
        acc.init_deps();
        acc
    }

    fn init_deps(&mut self) {
        if let Ok(Some((dir, manifest))) = manifest::Manifest::find(&self.base_dir) {
            if let Some(deps) = &manifest.dependencies {
                for dep in deps {
                    if let Some(path_str) = &dep.path {
                        let dep_path = dir.join(path_str);
                        if dep_path.exists() {
                            self.dep_paths.insert(dep.name.clone(), dep_path);
                        }
                    }
                }
            }
            if let Ok(Some(lf)) = lockfile::Lockfile::load(&dir) {
                for entry in lf.package {
                    self.lock_entries.insert(entry.name.clone(), entry);
                }
            }
        }
    }

    fn module_key(&self, path: &Path) -> String {
        path.strip_prefix(&self.base_dir)
            .or_else(|_| path.strip_prefix(std::env::current_dir().unwrap_or_default()))
            .unwrap_or(path)
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/")
    }
}

// ── Entry point: load main file + all transitive imports ───────────────

pub fn flow_load_main(acc: Acc, path: &Path) -> Result<(Acc, LoadedModule), String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("cannot resolve path {}: {}", path.display(), e))?;
    flow_load_file(acc, canonical)
}

/// Load a file (from cache or fresh) and all its transitive imports.
pub fn flow_load_file(mut acc: Acc, path: PathBuf) -> Result<(Acc, LoadedModule), String> {
    // Check cache first
    if acc.loaded.contains_key(&path) {
        let m = acc.loaded[&path].clone();
        return Ok((acc, m));
    }

    // Worklist as DFS stack. Each entry: (file_path, ancestor_chain).
    // ancestor_chain is the set of paths leading to this file (for cycle detection).
    let mut worklist: Vec<(PathBuf, Vec<PathBuf>)> = vec![(path.clone(), vec![path.clone()])];
    // Files that have been parsed (but whose imports may not be fully resolved yet).
    let mut parsed: HashMap<PathBuf, File> = HashMap::new();
    // Import edges for post-processing.
    let mut import_edges: Vec<ImportEdge> = Vec::new();

    while let Some((file_path, ancestors)) = worklist.pop() {
        if parsed.contains_key(&file_path) || acc.loaded.contains_key(&file_path) {
            continue;
        }

        let source = std::fs::read_to_string(&file_path)
            .map_err(|e| format!("failed to read {}: {}", file_path.display(), e))?;

        let tokens = lexer::Lexer::new(&source)
            .tokenize()
            .map_err(|e| format!("lexer error in {}: {}", file_path.display(), e))?;
        let file = parser::Parser::new(tokens)
            .parse_file()
            .map_err(|e| format!("parse error in {}: {}", file_path.display(), e))?;

        // Resolve imports for this file
        let mut resolved: Vec<ResolvedImport> = Vec::new();
        for import in &file.imports {
            let import_path = resolve_import_path(&file_path, &import.path, &acc)?;
            resolved.push((import.path.clone(), import_path.clone()));

            // Cycle check: is the import target in the current ancestor chain?
            if ancestors.contains(&import_path) {
                return Err(format!(
                    "circular dependency detected: {} imports itself",
                    import_path.display()
                ));
            }

            // Add to worklist if not already processed
            if !parsed.contains_key(&import_path) && !acc.loaded.contains_key(&import_path) {
                let mut child_ancestors = ancestors.clone();
                child_ancestors.push(import_path.clone());
                worklist.push((import_path, child_ancestors));
            }
        }
        import_edges.push((file_path.clone(), resolved));
        parsed.insert(file_path, file);
    }

    // Register all parsed files as LoadedModules
    for (file_path, file) in &parsed {
        let module_name = acc.module_key(file_path);
        let loaded = LoadedModule {
            path: file_path.clone(),
            file: file.clone(),
        };
        acc.modules.insert(module_name, loaded.clone());
        acc.loaded.insert(file_path.clone(), loaded);
    }

    // Process import edges: ensure each import's module is registered in modules map
    for (_parent_path, edges) in &import_edges {
        for (_, import_file_path) in edges {
            let dep_key = acc.module_key(import_file_path);
            if !acc.modules.contains_key(&dep_key) {
                if let Some(dep) = acc.loaded.get(import_file_path) {
                    acc.modules.insert(dep_key, dep.clone());
                }
            }
        }
    }

    // Return the main module
    let main_module = acc
        .loaded
        .get(&path)
        .cloned()
        .ok_or_else(|| "main file was not loaded".to_string())?;

    Ok((acc, main_module))
}

// ── Import resolution (pure function, no &mut self) ────────────────────

fn resolve_import_path(from: &Path, import_path: &[String], acc: &Acc) -> Result<PathBuf, String> {
    for segment in import_path {
        if segment == ".." || segment.contains('/') || segment.contains('\\') {
            return Err(format!(
                "import path '{}' contains invalid segment '{}'",
                import_path.join("::"),
                segment
            ));
        }
    }
    let base = from.parent().unwrap_or(&acc.base_dir);
    let relative: PathBuf = import_path.iter().collect();

    let try_paths = |paths: &[PathBuf]| -> Option<PathBuf> {
        for p in paths {
            if p.exists() {
                return Some(p.clone());
            }
        }
        None
    };

    // 1. Relative to importing file
    let file_path = base.join(&relative).with_extension("mimi");
    if file_path.exists() {
        return Ok(file_path);
    }

    // 2. Relative to base_dir
    let base_path = acc.base_dir.join(&relative).with_extension("mimi");
    if base_path.exists() {
        return Ok(base_path);
    }

    // 3. Dependency paths
    if let Some(first) = import_path.first() {
        if let Some(dep_dir) = acc.dep_paths.get(first) {
            let dep_relative: PathBuf = import_path.iter().skip(1).collect();
            let dep_path = dep_dir.join(&dep_relative).with_extension("mimi");
            if dep_path.exists() {
                return Ok(dep_path);
            }
            let dep_root = dep_dir.with_extension("mimi");
            if dep_root.exists() && import_path.len() == 1 {
                return Ok(dep_root);
            }
            if import_path.len() == 1 && dep_dir.is_dir() {
                if let Ok(Some((manifest_dir, manifest))) = manifest::Manifest::find(dep_dir) {
                    let entry_path = manifest.entry_path(&manifest_dir);
                    if entry_path.exists() {
                        return Ok(entry_path);
                    }
                }
            }
        }
    }

    // 4. .mimi/deps/
    let deps_dir = acc.base_dir.join(".mimi").join("deps");
    if deps_dir.exists() {
        if let Some(first) = import_path.first() {
            let dep_root = deps_dir.join(first);
            if dep_root.exists() {
                let dep_relative: PathBuf = import_path.iter().skip(1).collect();
                if let Some(found) = try_paths(&[
                    dep_root.join(&dep_relative).with_extension("mimi"),
                    dep_root.with_extension("mimi"),
                ]) {
                    if import_path.len() == 1 || found != dep_root.with_extension("mimi") {
                        return Ok(found);
                    }
                }
            }
        }
    }

    // 5. stdlib
    if let Some(std_dir) = super::stdlib_dir() {
        // With "std" prefix
        if import_path.first().map(|s| s == "std").unwrap_or(false) {
            if let Some(found) = try_paths(&[
                std_dir.join(&relative).with_extension("mimi"),
                std_dir
                    .join(import_path.iter().skip(1).collect::<PathBuf>())
                    .with_extension("mimi"),
            ]) {
                return Ok(found);
            }
        }
        // Without prefix
        if let Some(found) = try_paths(&[std_dir.join(&relative).with_extension("mimi")]) {
            return Ok(found);
        }
    }

    // 6. Selective import (use foo::bar → resolve foo.mimi)
    if import_path.len() >= 2 {
        let prefix: PathBuf = import_path[..import_path.len() - 1].iter().collect();
        // Relative to parent
        if let Some(found) = try_paths(&[
            base.join(&prefix).with_extension("mimi"),
            acc.base_dir.join(&prefix).with_extension("mimi"),
        ]) {
            return Ok(found);
        }
        // Dependency paths for selective
        if let Some(first) = import_path.first() {
            if let Some(dep_dir) = acc.dep_paths.get(first) {
                let dep_prefix: PathBuf = import_path[1..import_path.len() - 1].iter().collect();
                let dep_path = dep_dir.join(&dep_prefix).with_extension("mimi");
                if dep_path.exists() {
                    return Ok(dep_path);
                }
                if dep_prefix.as_os_str().is_empty() && dep_dir.is_dir() {
                    if let Ok(Some((manifest_dir, manifest))) = manifest::Manifest::find(dep_dir) {
                        let entry_path = manifest.entry_path(&manifest_dir);
                        if entry_path.exists() {
                            return Ok(entry_path);
                        }
                    }
                }
            }
        }
        // stdlib for selective
        if let Some(std_dir) = super::stdlib_dir() {
            if import_path.first().map(|s| s == "std").unwrap_or(false) {
                let sub_prefix: PathBuf = import_path[1..import_path.len() - 1].iter().collect();
                if let Some(found) = try_paths(&[std_dir.join(&sub_prefix).with_extension("mimi")])
                {
                    return Ok(found);
                }
            }
            if let Some(found) = try_paths(&[std_dir.join(&prefix).with_extension("mimi")]) {
                return Ok(found);
            }
        }
    }

    Err(format!(
        "cannot find module '{}' (looked in {}, {}, and stdlib)",
        import_path.join("::"),
        base.display(),
        acc.base_dir.display()
    ))
}

// ── merge_all — same logic as legacy, operates on &Acc ─────────────────

pub fn flow_merge_all(modules: &HashMap<String, LoadedModule>) -> Result<File, String> {
    let mut all_items = Vec::new();
    let mut seen_imports = HashSet::new();
    let mut all_imports = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    for module in modules.values() {
        for item in &module.file.items {
            if let Some(name) = item_name(item) {
                if !seen_names.insert(name.to_string()) {
                    let dup_modules: Vec<String> = modules
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

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::legacy::LegacyLoader;
    use std::fs;
    use std::path::Path;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mimi_test_flow_loader_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn cleanup(dir: &PathBuf) {
        let _ = fs::remove_dir_all(dir);
    }

    /// Compare flow loader result with legacy loader result for equivalence.
    fn assert_load_equivalent(dir: &Path, path: &Path) {
        // Flow version
        let acc = Acc::new(dir.to_path_buf());
        let (acc, flow_main) = flow_load_main(acc, path).expect("flow_load_main failed");
        let flow_modules = acc.modules.clone();

        // Legacy version
        let mut legacy = LegacyLoader::new(dir.to_path_buf());
        let legacy_main = legacy.load_main(path).expect("legacy load_main failed");
        let legacy_modules = legacy.modules.clone();

        // Compare main module
        assert_eq!(
            format!("{:?}", flow_main.file),
            format!("{:?}", legacy_main.file),
            "main module AST mismatch"
        );

        // Compare module set
        let flow_keys: HashSet<_> = flow_modules.keys().collect();
        let legacy_keys: HashSet<_> = legacy_modules.keys().collect();
        assert_eq!(flow_keys, legacy_keys, "module key set mismatch");
        for key in flow_keys {
            assert_eq!(
                format!("{:?}", flow_modules[key].file),
                format!("{:?}", legacy_modules[key].file),
                "file mismatch for module '{}'",
                key
            );
        }
    }

    // ── Single file ──────────────────────────────────────────────────

    #[test]
    fn test_flow_loader_single_file() {
        let dir = temp_dir("single");
        let file_path = dir.join("main.mimi");
        fs::write(&file_path, "func main() -> i32 { 42 }").unwrap();
        assert_load_equivalent(&dir, &file_path);
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_single_file_with_import() {
        let dir = temp_dir("single_import");
        fs::write(
            dir.join("main.mimi"),
            "use lib;\nfunc main() -> i32 { lib::helper() }",
        )
        .unwrap();
        fs::write(dir.join("lib.mimi"), "pub func helper() -> i32 { 7 }").unwrap();
        assert_load_equivalent(&dir, &dir.join("main.mimi"));
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_transitive_imports() {
        let dir = temp_dir("transitive");
        fs::write(
            dir.join("main.mimi"),
            "use a;\nfunc main() -> i32 { a::foo() }",
        )
        .unwrap();
        fs::write(
            dir.join("a.mimi"),
            "use b;\npub func foo() -> i32 { b::bar() }",
        )
        .unwrap();
        fs::write(dir.join("b.mimi"), "pub func bar() -> i32 { 99 }").unwrap();
        assert_load_equivalent(&dir, &dir.join("main.mimi"));
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_nonexistent_file() {
        let dir = temp_dir("nonexist");
        let file_path = dir.join("nope.mimi");
        let acc = Acc::new(dir.clone());
        let result = flow_load_main(acc, &file_path);
        assert!(
            result.is_err(),
            "loading nonexistent file should fail: {:?}",
            result.err()
        );
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_circular_dependency() {
        let dir = temp_dir("circular");
        fs::write(dir.join("a.mimi"), "use b;\npub func foo() -> i32 { 1 }").unwrap();
        fs::write(dir.join("b.mimi"), "use a;\npub func bar() -> i32 { 2 }").unwrap();
        let acc = Acc::new(dir.clone());
        let result = flow_load_main(acc, &dir.join("a.mimi"));
        assert!(result.is_err(), "circular dependency should fail");
        let err = format!("{:?}", result.err());
        assert!(
            err.contains("circular"),
            "error should mention circular dependency: {}",
            err
        );
        cleanup(&dir);
    }

    #[test]
    fn test_flow_loader_deep_diamond() {
        let dir = temp_dir("diamond");
        // A imports B and C; B imports D; C imports D
        fs::write(
            dir.join("a.mimi"),
            "use b;\nuse c;\nfunc main() -> i32 { b::f1() + c::f2() }",
        )
        .unwrap();
        fs::write(
            dir.join("b.mimi"),
            "use d;\npub func f1() -> i32 { d::f3() }",
        )
        .unwrap();
        fs::write(
            dir.join("c.mimi"),
            "use d;\npub func f2() -> i32 { d::f3() }",
        )
        .unwrap();
        fs::write(dir.join("d.mimi"), "pub func f3() -> i32 { 10 }").unwrap();
        assert_load_equivalent(&dir, &dir.join("a.mimi"));
        cleanup(&dir);
    }

    // ── merge_all ───────────────────────────────────────────────────

    #[test]
    fn test_flow_merge_all_empty() {
        let modules = HashMap::new();
        let file = flow_merge_all(&modules).unwrap();
        assert!(file.imports.is_empty());
        assert!(file.items.is_empty());
    }

    #[test]
    fn test_flow_merge_all_duplicate_detection() {
        use crate::ast::{File, FuncDef, Item};
        let mut modules = HashMap::new();
        let item = Item::Func(FuncDef {
            name: "conflict".to_string(),
            pub_: false,
            params: vec![],
            ret: None,
            body: vec![],
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            pos: (1, 1),
        });
        let file1 = File {
            imports: vec![],
            items: vec![item.clone()],
            implicit_single: false,
        };
        let file2 = File {
            imports: vec![],
            items: vec![item],
            implicit_single: false,
        };
        modules.insert(
            "a".to_string(),
            LoadedModule {
                path: PathBuf::from("a.mimi"),
                file: file1,
            },
        );
        modules.insert(
            "b".to_string(),
            LoadedModule {
                path: PathBuf::from("b.mimi"),
                file: file2,
            },
        );
        let result = flow_merge_all(&modules);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("duplicate"));
    }
}
