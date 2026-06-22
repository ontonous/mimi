use std::fs;
use std::path::Path;

use crate::ast::Item;
use crate::{lexer, parser};

pub(crate) fn doc(path: &Path, format: &str) -> Result<(), String> {
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
                            if let crate::ast::Stmt::Desc(desc, _) = stmt {
                                println!("{}", desc);
                                println!();
                            }
                            if let crate::ast::Stmt::Rule(text, _) = stmt {
                                println!("rule: {}", text);
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
            return Err(format!("unsupported doc format: {}", format));
        }
    }

    Ok(())
}
