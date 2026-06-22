use crate::ast::Item;
use crate::{lexer, parser};

pub fn generate_markdown(source: &str) -> Result<String, String> {
    let tokens = lexer::Lexer::new(source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    let mut out = String::new();

    for item in &file.items {
        match item {
            Item::Func(f) => {
                let params: Vec<String> = f.params.iter()
                    .map(|p| format!("{}: {:?}", p.name, p.ty))
                    .collect();
                let ret = f.ret.as_ref().map(|t| format!(" -> {:?}", t)).unwrap_or_default();
                out.push_str(&format!("## `func {}({}){}`\n\n", f.name, params.join(", "), ret));
                for stmt in &f.body {
                    if let crate::ast::Stmt::Desc(desc, _) = stmt {
                        out.push_str(&format!("{}\n\n", desc));
                    }
                    if let crate::ast::Stmt::Rule(text, _) = stmt {
                        out.push_str(&format!("rule: {}\n\n", text));
                    }
                }
            }
            Item::Type(t) => {
                out.push_str(&format!("## `type {}`\n\n", t.name));
            }
            Item::Module(m) => {
                out.push_str(&format!("## `module {}`\n\n", m.name));
            }
            _ => {}
        }
    }

    Ok(out)
}
