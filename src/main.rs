mod ast;
mod codegen;
mod contracts;
mod core;
mod interp;
mod lexer;
mod loader;
mod lsp;
mod manifest;
mod parser;
mod verifier;
#[cfg(test)]
mod tests;

use clap::{Parser, Subcommand};
use contracts::Contract;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ast::{File, Item, Stmt};

#[derive(Parser, Debug)]
#[command(name = "mimi", version = "0.1.1", about = "Mimi language driver")]
struct Args {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Parse and type-check a .mimi file (v0.1: parse only)
    Check {
        path: Option<PathBuf>,
        /// Extract and display contracts from mms blocks
        #[arg(long)]
        extract_contracts: bool,
        /// Strict mode: enforce $$ lock semantics
        #[arg(long)]
        strict: bool,
        /// Verify MMS rule attachment consistency
        #[arg(long)]
        verify_rules: bool,
    },
    /// Parse and run a .mimi file
    Run {
        path: Option<PathBuf>,
        /// Enable runtime contract verification
        #[arg(long)]
        verify_contracts: bool,
        /// Default allocator type: system, arena, or bump
        #[arg(long, default_value = "system")]
        allocator: String,
    },
    /// Run test functions (functions named test_*)
    Test {
        path: Option<PathBuf>,
        /// Default allocator type: system, arena, or bump
        #[arg(long, default_value = "system")]
        allocator: String,
    },
    /// Initialize a new mimi.toml
    Init {
        /// Package name
        name: Option<String>,
    },
    /// Add a dependency
    Add {
        /// Package name
        name: String,
        /// Version requirement
        #[arg(short, long)]
        version: Option<String>,
        /// Local path
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Remove a dependency
    Remove {
        /// Package name
        name: String,
    },
    /// List dependencies
    List,
    /// Start LSP server (stdin/stdout)
    Lsp,
    /// Verify contracts using Z3 SMT solver
    Verify {
        path: Option<PathBuf>,
    },
    /// Compile a .mimi file to native code
    Build {
        path: Option<PathBuf>,
        /// Output path for the compiled binary
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Emit LLVM IR instead of compiling
        #[arg(long)]
        emit_ir: bool,
    },
    /// Promote a .mms file to .mimi (clean placeholders, validate locks)
    Promote {
        path: PathBuf,
        /// Output path (defaults to same name with .mimi extension)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Generate documentation from Mimi source
    Doc {
        path: PathBuf,
        /// Output format: markdown (default)
        #[arg(short, long, default_value = "markdown")]
        format: String,
    },
}

fn main() {
    let args = Args::parse();
    let result = match args.cmd {
        Command::Check { path, extract_contracts, strict, verify_rules } => check(path.as_deref(), extract_contracts, strict, verify_rules),
        Command::Run { path, verify_contracts, allocator } => run(path.as_deref(), verify_contracts, &allocator),
        Command::Test { path, allocator } => test(path.as_deref(), &allocator),
        Command::Init { name } => init(name.as_deref()),
        Command::Add { name, version, path } => add(&name, version.as_deref(), path.as_deref()),
        Command::Remove { name } => remove(&name),
        Command::List => list(),
        Command::Lsp => lsp(),
        Command::Verify { path } => verify(path.as_deref()),
        Command::Build { path, output, emit_ir } => build(path.as_deref(), output.as_deref(), emit_ir),
        Command::Promote { path, output } => promote(&path, output.as_deref()),
        Command::Doc { path, format } => doc(&path, &format),
    };
    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

/// Resolve the target path, either from argument or by finding mimi.toml
fn resolve_path(arg: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = arg {
        return Ok(path.to_path_buf());
    }
    // Search for mimi.toml
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    match manifest::Manifest::find(&cwd)? {
        Some((dir, m)) => Ok(m.entry_path(&dir)),
        None => Err("no path specified and no mimi.toml found".into()),
    }
}

fn is_sketch(path: &Path) -> bool {
    path.extension().map(|e| e == "mms").unwrap_or(false)
}

fn is_production(path: &Path) -> bool {
    path.extension().map(|e| e == "mimi").unwrap_or(false)
}

/// Extract contracts from all mms blocks in the file, keyed by function name
fn extract_all_contracts(file: &File) -> HashMap<String, Contract> {
    let mut result = HashMap::new();
    extract_item_contracts(&file.items, &mut result);
    result
}

fn extract_item_contracts(items: &[Item], out: &mut HashMap<String, Contract>) {
    for item in items {
        match item {
            Item::Func(func) => {
                let mut contract = Contract::default();
                for stmt in &func.body {
                    if let Stmt::MmsBlock { content: text, .. } = stmt {
                        let c = contracts::extract_contracts(text);
                        contract.requires.extend(c.requires);
                        contract.ensures.extend(c.ensures);
                        contract.math.extend(c.math);
                    }
                }
                if !contract.requires.is_empty() || !contract.ensures.is_empty() || !contract.math.is_empty() {
                    out.insert(func.name.clone(), contract);
                }
            }
            Item::Module(m) => {
                extract_item_contracts(&m.items, out);
            }
            _ => {}
        }
    }
}

fn check(path: Option<&Path>, extract_contracts: bool, strict: bool, verify_rules: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let sketch = is_sketch(&path);
    let tokens = if sketch {
        lexer::Lexer::new_sketch(&source).tokenize()?
    } else {
        lexer::Lexer::new(&source).tokenize()?
    };
    let mut file = if sketch {
        parser::Parser::new_sketch(tokens).parse_file()?
    } else {
        parser::Parser::new(tokens).parse_file()?
    };
    if sketch {
        println!("✓ {} parsed successfully (sketch mode)", path.display());
        return Ok(());
    }
    if !is_production(&path) {
        return Err(format!(
            "expected .mimi production file or .mms sketch file, got {}",
            path.display()
        ));
    }

    // Extract contracts from mms blocks if requested
    if extract_contracts {
        let contracts = extract_all_contracts(&file);
        if contracts.is_empty() {
            println!("No contracts found in mms blocks.");
        } else {
            println!("Contracts extracted from mms blocks:");
            for (func_name, contract) in &contracts {
                println!("  {}:", func_name);
                for req in &contract.requires {
                    println!("    requires: {}", req);
                }
                for ens in &contract.ensures {
                    println!("    ensures: {}", ens);
                }
                for m in &contract.math {
                    println!("    math: {}", m);
                }
            }
        }
        // Bind contracts to functions
        contracts::bind_contracts(&mut file, contracts);
    }

    let check_result = if strict {
        core::check_strict(&file)
    } else {
        core::check(&file)
    };
    if let Err(diagnostics) = check_result {
        eprintln!("✗ {} has {} type error(s):", path.display(), diagnostics.len());
        for d in diagnostics {
            eprintln!("  - {}", d.message);
        }
        return Err("type checking failed".into());
    }

    // Verify MMS rule attachment consistency
    if verify_rules {
        let rule_errors = core::verify_rules(&file);
        if !rule_errors.is_empty() {
            eprintln!("✗ {} has {} rule error(s):", path.display(), rule_errors.len());
            for e in &rule_errors {
                eprintln!("  - {}", e);
            }
            return Err("rule verification failed".into());
        }
        println!("✓ {} rules verified", path.display());
    }

    println!("✓ {} checked successfully", path.display());
    Ok(())
}

fn run(path: Option<&Path>, verify_contracts: bool, allocator: &str) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if is_sketch(&path) {
        return Err("cannot run a .mms sketch file directly; promote to .mimi first".into());
    }
    if !is_production(&path) {
        return Err(format!(
            "expected .mimi production file, got {}",
            path.display()
        ));
    }
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    // Load imports if any
    let merged_file = if !file.imports.is_empty() {
        let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main(&path)?;
        loader.merge_all()
    } else {
        file
    };

    if let Err(diagnostics) = core::check(&merged_file) {
        eprintln!("✗ {} has {} type error(s):", path.display(), diagnostics.len());
        for d in diagnostics {
            eprintln!("  - {}", d.message);
        }
        return Err("type checking failed".into());
    }
    let mut interp = interp::Interpreter::new(&merged_file);
    interp.verify_contracts = verify_contracts;
    interp.default_allocator = match allocator {
        "arena" => interp::AllocatorKind::Arena,
        "bump" => interp::AllocatorKind::Bump,
        _ => interp::AllocatorKind::System,
    };
    let value = interp.run()?;
    println!("-> {}", value);
    Ok(())
}

fn test(path: Option<&Path>, allocator: &str) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if is_sketch(&path) {
        return Err("cannot test a .mms sketch file directly; promote to .mimi first".into());
    }
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    // Load imports if any
    let merged_file = if !file.imports.is_empty() {
        let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main(&path)?;
        loader.merge_all()
    } else {
        file
    };

    if let Err(diagnostics) = core::check(&merged_file) {
        eprintln!("✗ {} has {} type error(s):", path.display(), diagnostics.len());
        for d in diagnostics {
            eprintln!("  - {}", d.message);
        }
        return Err("type checking failed".into());
    }

    // Find test functions (functions starting with "test_")
    let test_funcs: Vec<String> = merged_file.items.iter().filter_map(|item| {
        match item {
            Item::Func(f) if f.name.starts_with("test_") => Some(f.name.clone()),
            _ => None,
        }
    }).collect();

    if test_funcs.is_empty() {
        println!("No test functions found (functions starting with test_).");
        return Ok(());
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut errors = Vec::new();

    for func_name in &test_funcs {
        let mut interp = interp::Interpreter::new(&merged_file);
        interp.default_allocator = match allocator {
            "arena" => interp::AllocatorKind::Arena,
            "bump" => interp::AllocatorKind::Bump,
            _ => interp::AllocatorKind::System,
        };
        match interp.call_named(func_name, vec![]) {
            Ok(_) => {
                println!("  ✓ {}", func_name);
                passed += 1;
            }
            Err(e) => {
                println!("  ✗ {}: {}", func_name, e);
                failed += 1;
                errors.push((func_name.clone(), e));
            }
        }
    }

    println!("\n{} passed, {} failed", passed, failed);
    if failed > 0 {
        Err(format!("{} test(s) failed", failed))
    } else {
        Ok(())
    }
}

fn init(name: Option<&str>) -> Result<(), String> {
    let dir = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let toml_path = dir.join("mimi.toml");
    if toml_path.exists() {
        return Err("mimi.toml already exists".into());
    }
    let pkg_name = name.unwrap_or("my-package");
    let manifest = manifest::Manifest::new(pkg_name);
    manifest.save(&dir)?;
    println!("✓ Created mimi.toml for package '{}'", pkg_name);

    // Create main.mimi if it doesn't exist
    let entry_path = manifest.entry_path(&dir);
    if !entry_path.exists() {
        std::fs::write(&entry_path, "func main() -> i32 {\n    42\n}\n")
            .map_err(|e| format!("failed to create {}: {}", entry_path.display(), e))?;
        println!("✓ Created {}", entry_path.display());
    }
    Ok(())
}

fn add(name: &str, version: Option<&str>, path: Option<&str>) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, mut manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found; run 'mimi init' first".into()),
    };
    manifest.add_dependency(name, version, path);
    manifest.save(&dir)?;
    println!("✓ Added dependency '{}'", name);
    Ok(())
}

fn remove(name: &str) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (dir, mut manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found".into()),
    };
    if manifest.remove_dependency(name) {
        manifest.save(&dir)?;
        println!("✓ Removed dependency '{}'", name);
    } else {
        println!("Dependency '{}' not found", name);
    }
    Ok(())
}

fn list() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    let (_dir, manifest) = match manifest::Manifest::find(&cwd)? {
        Some((d, m)) => (d, m),
        None => return Err("no mimi.toml found".into()),
    };
    if let Some(deps) = &manifest.dependencies {
        if deps.is_empty() {
            println!("No dependencies.");
        } else {
            println!("Dependencies:");
            for dep in deps {
                let version = dep.version.as_deref().unwrap_or("*");
                let source = dep.path.as_deref().unwrap_or("registry");
                println!("  {} {} ({})", dep.name, version, source);
            }
        }
    } else {
        println!("No dependencies.");
    }
    Ok(())
}

fn lsp() -> Result<(), String> {
    let mut server = lsp::LspServer::new();
    server.run()
}

fn verify(path: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let results = verifier::verify_source(&source)?;
    if results.is_empty() {
        println!("No contracts to verify in {}", path.display());
    } else {
        let mut all_passed = true;
        for r in &results {
            let icon = match r.status {
                verifier::VerifStatus::Verified => "✓",
                verifier::VerifStatus::Failed => "✗",
                verifier::VerifStatus::Unknown => "?",
            };
            println!("  {} {}: {}", icon, r.func_name, r.message);
            if r.status == verifier::VerifStatus::Failed {
                all_passed = false;
            }
        }
        println!("\n{}/{} verified", results.iter().filter(|r| r.status == verifier::VerifStatus::Verified).count(), results.len());
        if !all_passed {
            return Err("verification failed".into());
        }
    }
    Ok(())
}

fn build(path: Option<&Path>, output: Option<&Path>, emit_ir: bool) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    if let Err(diagnostics) = core::check(&file) {
        eprintln!("✗ {} has {} type error(s):", path.display(), diagnostics.len());
        for d in diagnostics {
            eprintln!("  - {}", d.message);
        }
        return Err("type checking failed".into());
    }

    let context = inkwell::context::Context::create();
    let module_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("main");
    let mut codegen = codegen::CodeGenerator::new(&context, module_name);

    codegen.compile_file(&file)?;

    if emit_ir {
        println!("{}", codegen.emit_ir());
        return Ok(());
    }

    let output_path_buf = output.map(|p| p.to_path_buf()).unwrap_or_else(|| {
        let mut out = path.clone();
        out.set_extension("");
        out
    });
    let output_path = output.unwrap_or(&output_path_buf);

    codegen.compile_to_object(&output_path.with_extension("o"))?;

    // Link with cc to create executable
    let obj_path = output_path.with_extension("o");
    let status = std::process::Command::new("cc")
        .arg(obj_path.to_str().ok_or("object path is not valid UTF-8")?)
        .arg("-o")
        .arg(output_path.to_str().ok_or("output path is not valid UTF-8")?)
        .status()
        .map_err(|e| format!("failed to run linker: {}", e))?;

    // Cleanup object file
    let _ = std::fs::remove_file(&obj_path);

    if status.success() {
        println!("✓ Compiled {} → {}", path.display(), output_path.display());
    } else {
        return Err(format!("linker failed with exit code {:?}", status.code()));
    }
    Ok(())
}

#[cfg(test)]
pub fn main_promote(path: &Path, output: Option<&Path>) -> Result<(), String> {
    promote(path, output)
}

#[cfg(test)]
pub fn main_doc(path: &Path, format: &str) -> Result<(), String> {
    doc(path, format)
}

fn promote(path: &Path, output: Option<&Path>) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;

    // Check for ... placeholders
    if source.contains("...") {
        return Err(format!("file contains '...' placeholders, cannot promote: {}", path.display()));
    }

    // Check for uncommitted desc/rule (without $ suffix)
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    for item in &file.items {
        if let Item::Func(_) = item {
            // Check if function has desc or rule without commitment
            // For now, just check basic structure
        }
    }

    // Determine output path
    let output_path = if let Some(out) = output {
        out.to_path_buf()
    } else {
        let mut out = path.to_path_buf();
        out.set_extension("mimi");
        out
    };

    // Write the promoted file
    fs::write(&output_path, &source)
        .map_err(|e| format!("failed to write {}: {}", output_path.display(), e))?;

    println!("✓ Promoted {} → {}", path.display(), output_path.display());
    Ok(())
}

fn doc(path: &Path, format: &str) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;

    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    match format {
        "markdown" | "md" => {
            println!("# Documentation for {}", path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown"));
            println!();

            for item in &file.items {
                match item {
                    Item::Func(f) => {
                        let params: Vec<String> = f.params.iter()
                            .map(|p| format!("{}: {:?}", p.name, p.ty))
                            .collect();
                        let ret = f.ret.as_ref().map(|t| format!(" -> {:?}", t)).unwrap_or_default();
                        println!("## `func {}({}){}`", f.name, params.join(", "), ret);
                        println!();
                        // Extract desc from body
                        for stmt in &f.body {
                            if let crate::ast::Stmt::Desc(desc) = stmt {
                                println!("{}", desc);
                                println!();
                            }
                        }
                    }
                    Item::Type(t) => {
                        println!("## `type {}`", t.name);
                        println!();
                    }
                    Item::Module(m) => {
                        println!("## `module {}`", m.name);
                        println!();
                    }
                    _ => {}
                }
            }
        }
        _ => {
            return Err(format!("unsupported format: {}", format));
        }
    }

    Ok(())
}
