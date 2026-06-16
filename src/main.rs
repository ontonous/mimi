#![allow(dead_code)]

mod ast;
mod core;
mod interp;
mod lexer;
mod loader;
mod parser;
#[cfg(test)]
mod tests;

use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "mimi", version = "0.1.1", about = "Mimi language driver")]
struct Args {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Parse and type-check a .mimi file (v0.1: parse only)
    Check { path: PathBuf },
    /// Parse and run a .mimi file
    Run { path: PathBuf },
}

fn main() {
    let args = Args::parse();
    let result = match args.cmd {
        Command::Check { path } => check(&path),
        Command::Run { path } => run(&path),
    };
    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

fn is_sketch(path: &PathBuf) -> bool {
    path.extension().map(|e| e == "mms").unwrap_or(false)
}

fn is_production(path: &PathBuf) -> bool {
    path.extension().map(|e| e == "mimi").unwrap_or(false)
}

fn check(path: &PathBuf) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let sketch = is_sketch(path);
    let tokens = if sketch {
        lexer::Lexer::new_sketch(&source).tokenize()?
    } else {
        lexer::Lexer::new(&source).tokenize()?
    };
    let file = if sketch {
        parser::Parser::new_sketch(tokens).parse_file()?
    } else {
        parser::Parser::new(tokens).parse_file()?
    };
    if sketch {
        println!("✓ {} parsed successfully (sketch mode)", path.display());
        return Ok(());
    }
    if !is_production(path) {
        return Err(format!(
            "expected .mimi production file or .mms sketch file, got {}",
            path.display()
        ));
    }
    if let Err(diagnostics) = core::check(&file) {
        eprintln!("✗ {} has {} type error(s):", path.display(), diagnostics.len());
        for d in diagnostics {
            eprintln!("  - {}", d.message);
        }
        return Err("type checking failed".into());
    }
    println!("✓ {} checked successfully", path.display());
    Ok(())
}

fn run(path: &PathBuf) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if is_sketch(path) {
        return Err("cannot run a .mms sketch file directly; promote to .mimi first".into());
    }
    if !is_production(path) {
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
        loader.load_main(path)?;
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
    let value = interp.run()?;
    println!("-> {}", value);
    Ok(())
}
