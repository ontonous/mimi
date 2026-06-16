use crate::ast::*;
use crate::{core, lexer, parser};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Loaded module with its parsed AST and file path
#[derive(Clone)]
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
}

impl ModuleLoader {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            loaded: HashMap::new(),
            modules: HashMap::new(),
        }
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

        // Read and parse
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        let tokens = lexer::Lexer::new(&source).tokenize()
            .map_err(|e| format!("lexer error in {}: {}", path.display(), e))?;
        let file = parser::Parser::new(tokens).parse_file()
            .map_err(|e| format!("parse error in {}: {}", path.display(), e))?;

        let module_name = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let mut loaded = LoadedModule {
            path: path.to_path_buf(),
            file,
        };

        // Resolve imports
        let imports = loaded.file.imports.clone();
        for import in &imports {
            let import_path = self.resolve_import(path, &import.path)?;
            let dep = self.load_file(&import_path)?;
            let dep_name = import_path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            self.modules.insert(dep_name, dep);
        }

        self.modules.insert(module_name, loaded.clone());
        self.loaded.insert(path.to_path_buf(), loaded.clone());

        Ok(loaded)
    }

    /// Resolve a use path to a file path
    fn resolve_import(&self, from: &Path, path: &[String]) -> Result<PathBuf, String> {
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

        Err(format!(
            "cannot find module '{}' (looked in {} and {})",
            path.join("::"),
            base.display(),
            self.base_dir.display()
        ))
    }

    /// Get a loaded module by name
    pub fn get_module(&self, name: &str) -> Option<&LoadedModule> {
        self.modules.get(name)
    }

    /// Merge all loaded module items into a single file for interpretation
    pub fn merge_all(&self) -> File {
        let mut all_items = Vec::new();
        let mut all_imports = Vec::new();

        for module in self.modules.values() {
            all_items.extend(module.file.items.clone());
            all_imports.extend(module.file.imports.clone());
        }

        File {
            imports: all_imports,
            items: all_items,
        }
    }

    /// Type-check all loaded modules
    pub fn check_all(&self) -> Result<(), Vec<core::Diagnostic>> {
        for module in self.modules.values() {
            core::check(&module.file)?;
        }
        Ok(())
    }
}
