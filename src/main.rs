use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mimi::ast::{File, Item, Stmt};
use mimi::contracts::Contract;
use mimi::diagnostic::format::format_simple_error;

#[path = "main/add.rs"]
mod add;
#[path = "main/bindgen.rs"]
mod bindgen;
#[path = "main/build.rs"]
mod build;
#[path = "main/check.rs"]
mod check;
#[path = "main/doc.rs"]
mod doc;
#[path = "main/emit.rs"]
mod emit;
#[path = "main/fmt_cmd.rs"]
mod fmt_cmd;
#[path = "main/init.rs"]
mod init;
#[path = "main/install.rs"]
mod install;
#[path = "main/lint_cmd.rs"]
mod lint_cmd;
#[path = "main/list.rs"]
mod list;
#[path = "main/lsp_cmd.rs"]
mod lsp_cmd;
#[path = "main/mms.rs"]
mod mms;
#[path = "main/promote.rs"]
mod promote;
#[path = "main/publish.rs"]
mod publish;
#[path = "main/remove.rs"]
mod remove;
#[path = "main/run.rs"]
mod run;
#[path = "main/search.rs"]
mod search;
#[path = "main/stats.rs"]
mod stats;
#[path = "main/test.rs"]
mod test;
#[path = "main/tool_stat.rs"]
mod tool_stat;
#[path = "main/tree.rs"]
mod tree;
#[path = "main/update.rs"]
mod update;
#[path = "main/verify.rs"]
mod verify;

#[derive(Parser, Debug)]
#[command(name = "mimi", version = env!("CARGO_PKG_VERSION"), about = "Mimi language driver")]
struct Args {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Parse and type-check a .mimi file, reporting all type errors
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
        /// Enable FFI contract verification (requires/ensures checking); use --skip-verify-ffi to disable
        #[arg(long, default_value_t = true)]
        verify_ffi: bool,
        /// Skip FFI contract verification (overrides --verify-ffi)
        #[arg(long)]
        skip_verify_ffi: bool,
        /// Default allocator type: system, arena, or bump
        #[arg(long, default_value = "system")]
        allocator: String,
        /// Strict mode: only compile $/$$ locked fragments
        #[arg(long)]
        strict: bool,
        /// Profile mode: track function call counts and durations
        #[arg(long)]
        profile: bool,
        /// Watch mode: re-run on file changes
        #[arg(long, short)]
        watch: bool,
        /// Extra arguments forwarded to the program (after --)
        #[arg(trailing_var_arg = true)]
        extra_args: Vec<String>,
    },
    /// Run test functions (functions named test_*)
    Test {
        path: Option<PathBuf>,
        /// Default allocator type: system, arena, or bump
        #[arg(long, default_value = "system")]
        allocator: String,
        /// Filter tests by pattern (substring match)
        #[arg(long, short)]
        filter: Option<String>,
        /// Show verbose output for failed tests
        #[arg(long, short)]
        verbose: bool,
        /// Strict mode: only execute $/$$ locked test functions
        #[arg(long)]
        strict: bool,
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
        /// Git repository URL
        #[arg(long)]
        git: Option<String>,
        /// Git tag/branch (default: main)
        #[arg(long)]
        tag: Option<String>,
        /// Dry run: print the resolved version without writing the manifest
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a dependency
    Remove {
        /// Package name
        name: String,
    },
    /// List dependencies
    List,
    /// Show dependency tree
    Tree,
    /// Start LSP server (stdin/stdout)
    Lsp,
    /// Format .mimi files
    Fmt {
        /// File(s) to format; use - for stdin. If omitted, all .mimi files
        /// under the current project or working directory are formatted.
        files: Vec<PathBuf>,
        /// Check mode: exit with non-zero if formatting changes needed
        #[arg(long)]
        check: bool,
    },
    /// Lint .mimi files for common issues
    Lint {
        /// File(s) to lint
        files: Vec<PathBuf>,
        /// Treat warnings as errors (exit non-zero on warnings)
        #[arg(long)]
        fail_on_warnings: bool,
    },
    /// Verify contracts using Z3 SMT solver
    Verify {
        path: Option<PathBuf>,
        /// Show per-function verification statistics (constraints, solving time)
        #[arg(long)]
        stats: bool,
        /// Dump Z3 SMT-LIB2 assertions to stderr for debugging
        #[arg(long)]
        dump_z3: bool,
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
        /// Strict mode: only compile $/$$ locked fragments
        #[arg(long)]
        strict: bool,
        /// no_std mode: compile without libc (freestanding target)
        #[arg(long)]
        no_std: bool,
        /// Verify contracts: compile requires/ensures as runtime asserts
        #[arg(long)]
        verify_contracts: bool,
        /// Verify extern call sites satisfy preconditions (Z3)
        #[arg(long)]
        verify_ffi: bool,
        /// Build as shared library (.so) instead of executable
        #[arg(long)]
        shared: bool,
        /// Target triple for cross-compilation (e.g. x86_64-pc-windows-gnu)
        #[arg(long)]
        target: Option<String>,
    },
    /// Generate C header file from extern declarations
    EmitCHeaders {
        path: Option<PathBuf>,
        /// Output path for the C header file
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Generate Python pybind11 bindings from extern declarations
    EmitPyBindings {
        path: Option<PathBuf>,
        /// Output path for the pybind11 C++ source file
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Path to the compiled Mimi shared library (.so) for CMakeLists.txt linking
        #[arg(long)]
        mimi_lib: Option<PathBuf>,
    },
    /// Generate Rust FFI bindings from extern declarations
    EmitRustBindings {
        path: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Generate Go CGO bindings from extern declarations
    EmitGoBindings {
        path: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Generate Node.js N-API bindings from extern declarations
    EmitNodeBindings {
        path: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Output path for TypeScript .d.ts type declarations
        #[arg(long)]
        ts: Option<PathBuf>,
    },
    /// Generate C++ RAII bindings from extern declarations
    EmitCppBindings {
        path: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Generate Java JNI bindings from extern declarations
    EmitJavaBindings {
        path: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Output path for Java interface class
        #[arg(long)]
        java: Option<PathBuf>,
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
        /// Output format: markdown (default), mms
        #[arg(short, long, default_value = "markdown")]
        format: String,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Analyze directory statistics (files, dirs, extensions)
    Stat {
        /// Directory to analyze (default: current directory)
        path: Option<PathBuf>,
        /// Recursive scan depth (default: 1)
        #[arg(short, long, default_value_t = 1)]
        depth: u32,
        /// Show SHA-256 hashes of files
        #[arg(long)]
        hash: bool,
    },
    /// Generate FFI bindings for all supported languages
    Bindgen {
        /// .mimi file with extern declarations
        path: PathBuf,
        /// Output directory for generated bindings
        #[arg(short, long, default_value = "bindings")]
        output: PathBuf,
    },
    /// Parse and process .mms files (MimiSpec)
    Mms {
        /// .mms file(s) to parse; use - for stdin
        files: Vec<PathBuf>,

        /// Show AST structure
        #[arg(short, long)]
        ast: bool,

        /// Output results as JSON (useful for editor integrations)
        #[arg(short, long)]
        json: bool,

        /// Render AST back to MimiSpec source
        #[arg(short, long)]
        render: bool,

        /// Render math as LaTeX
        #[arg(short, long)]
        latex: bool,
    },
    /// Display Mimi usage statistics
    Stats { path: Option<PathBuf> },
    /// Install dependencies from mimi.toml
    Install {
        /// Install all dependencies (default; reserved for future per-target filters)
        #[arg(long)]
        all: bool,
        /// Frozen mode: do not update lockfile, fail if any dep is missing from cache
        #[arg(long)]
        frozen: bool,
        /// Offline mode: only use cached `.mimi/deps`; never fetch from network or git
        #[arg(long)]
        offline: bool,
    },
    /// Update dependencies to latest compatible versions
    Update,
    /// Publish package to local registry
    Publish {
        /// Package name (defaults to mimi.toml name)
        #[arg(short, long)]
        name: Option<String>,
        /// Version (defaults to mimi.toml version)
        #[arg(short, long)]
        version: Option<String>,
    },
    /// Search for packages in registry
    Search {
        /// Search query
        query: String,
    },
}

fn main() {
    let args = Args::parse();
    let result = match args.cmd {
        Command::Check {
            path,
            extract_contracts,
            strict,
            verify_rules,
        } => check::check(path.as_deref(), extract_contracts, strict, verify_rules),
        Command::Run {
            path,
            verify_contracts,
            verify_ffi,
            skip_verify_ffi,
            allocator,
            strict,
            watch,
            profile,
            extra_args,
        } => {
            let ffi_check = verify_ffi && !skip_verify_ffi;
            run::run(
                path.as_deref(),
                verify_contracts,
                ffi_check,
                &allocator,
                strict,
                watch,
                profile,
                &extra_args,
            )
        }
        Command::Test {
            path,
            allocator,
            filter,
            verbose,
            strict,
        } => test::test(
            path.as_deref(),
            &allocator,
            filter.as_deref(),
            verbose,
            strict,
        ),
        Command::Init { name } => match std::env::current_dir() {
            Ok(cwd) => init::init(&cwd, name.as_deref()),
            Err(e) => Err(format!("cannot get cwd: {}", e)),
        },
        Command::Add {
            name,
            version,
            path,
            git,
            tag,
            dry_run,
        } => add::add(
            &name,
            version.as_deref(),
            path.as_deref(),
            git.as_deref(),
            tag.as_deref(),
            dry_run,
        ),
        Command::Remove { name } => remove::remove(&name),
        Command::List => list::list(),
        Command::Tree => tree::tree(),
        Command::Lsp => lsp_cmd::lsp(),
        Command::Fmt { files, check } => fmt_cmd::fmt_files(&files, check),
        Command::Lint {
            files,
            fail_on_warnings,
        } => lint_cmd::lint_files(&files, fail_on_warnings),
        Command::Verify {
            path,
            stats,
            dump_z3,
        } => verify::verify(path.as_deref(), stats, dump_z3),
        Command::Build {
            path,
            output,
            emit_ir,
            strict,
            no_std,
            verify_contracts,
            verify_ffi,
            shared,
            target,
        } => build::build(
            path.as_deref(),
            output.as_deref(),
            emit_ir,
            strict,
            no_std,
            verify_contracts,
            verify_ffi,
            shared,
            target.as_deref(),
        ),
        Command::EmitCHeaders { path, output } => {
            emit::emit_c_headers(path.as_deref(), output.as_deref())
        }
        Command::EmitPyBindings {
            path,
            output,
            mimi_lib,
        } => emit::emit_py_bindings(path.as_deref(), output.as_deref(), mimi_lib.as_deref()),
        Command::EmitRustBindings { path, output } => {
            emit::emit_rust_bindings(path.as_deref(), output.as_deref())
        }
        Command::EmitGoBindings { path, output } => {
            emit::emit_go_bindings(path.as_deref(), output.as_deref())
        }
        Command::EmitNodeBindings { path, output, ts } => {
            emit::emit_node_bindings(path.as_deref(), output.as_deref(), ts.as_deref())
        }
        Command::EmitCppBindings { path, output } => {
            emit::emit_cpp_bindings(path.as_deref(), output.as_deref())
        }
        Command::EmitJavaBindings { path, output, java } => {
            emit::emit_java_bindings(path.as_deref(), output.as_deref(), java.as_deref())
        }
        Command::Promote { path, output } => promote::promote(&path, output.as_deref()),
        Command::Doc {
            path,
            format,
            output,
        } => doc::doc(&path, &format, output.as_deref()),
        Command::Mms {
            files,
            ast,
            json,
            render,
            latex,
        } => mms::mms(&files, ast, json, render, latex),
        Command::Stats { path } => stats::stats(path.as_deref()),
        Command::Stat { path, depth, hash } => {
            let dir = path
                .as_deref()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            tool_stat::run(&dir, depth, hash)
        }
        Command::Bindgen { path, output } => bindgen::run(&path, &output),
        Command::Install {
            all: _,
            frozen,
            offline,
        } => install::install(frozen, offline),
        Command::Update => update::update(),
        Command::Publish { name, version } => publish::publish(name.as_deref(), version.as_deref()),
        Command::Search { query } => search::search(&query),
    };
    if let Err(e) = result {
        eprintln!("{}", format_simple_error(&e));
        std::process::exit(1);
    }
}

/// Resolve the target path, either from argument or by finding mimi.toml
pub(crate) fn resolve_path(arg: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = arg {
        return Ok(path.to_path_buf());
    }
    // Search for mimi.toml
    let cwd = std::env::current_dir().map_err(|e| format!("cannot get cwd: {}", e))?;
    match mimi::manifest::Manifest::find(&cwd)? {
        Some((dir, m)) => Ok(m.entry_path(&dir)),
        None => Err("no path specified and no mimi.toml found".into()),
    }
}

pub(crate) fn is_sketch(path: &Path) -> bool {
    path.extension().map(|e| e == "mms").unwrap_or(false)
}

pub(crate) fn is_production(path: &Path) -> bool {
    path.extension().map(|e| e == "mimi").unwrap_or(false)
}

/// Extract contracts from all mms blocks in the file, keyed by function name
pub(crate) fn extract_all_contracts(file: &File) -> HashMap<String, Contract> {
    let mut result = HashMap::new();
    extract_item_contracts(&file.items, &mut result);
    result
}

pub(crate) fn extract_item_contracts(items: &[Item], out: &mut HashMap<String, Contract>) {
    for item in items {
        match item {
            Item::Func(func) => {
                let mut contract = Contract::default();
                for stmt in &func.body {
                    if let Stmt::MmsBlock {
                        content: text,
                        span,
                        ..
                    } = stmt
                    {
                        let c = mimi::contracts::extract_contracts_with_span(text, *span);
                        contract.requires.extend(c.requires);
                        contract.ensures.extend(c.ensures);
                        contract.math.extend(c.math);
                        contract.span = *span;
                    }
                }
                if !contract.requires.is_empty()
                    || !contract.ensures.is_empty()
                    || !contract.math.is_empty()
                {
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
