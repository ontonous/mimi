use crate::ast::Item;
use crate::{lexer, parser};

/// Generate Markdown from a .mimi source (Mimi parser).
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

/// Generate Markdown from a .mms source (MimiSpec parser).
pub fn generate_markdown_from_mms(source: &str) -> Result<String, String> {
    use mimispec::ast::*;
    use mimispec::parse;

    let result = parse(source);
    let file = result.file;

    let mut out = String::new();

    for frag in &file.fragments {
        append_fragment_markdown(frag, &mut out);
    }

    Ok(out)
}

fn append_fragment_markdown(frag: &mimispec::ast::Fragment, out: &mut String) {
    use mimispec::ast::*;
    match frag {
        Fragment::Module { module } => {
            out.push_str(&format!("## `module {}`\n\n", module.name.name));
            if let Some(d) = &module.desc {
                out.push_str(&format!("{}\n\n", d.content.value));
            }
            for r in &module.rules {
                out.push_str(&format!("rule: {}\n\n", r.desc.content.value));
            }
            for item in &module.items {
                append_fragment_markdown(item, out);
            }
        }
        Fragment::Func { func } => {
            let params: Vec<String> = func.params.iter()
                .map(|p| p.name.name.clone())
                .collect();
            out.push_str(&format!("## `func {}({})`\n\n", func.name.name, params.join(", ")));
            if let Some(d) = &func.desc {
                out.push_str(&format!("{}\n\n", d.content.value));
            }
            if let Some(cond) = &func.requires {
                if let Condition::Structured { expr } = cond {
                    out.push_str(&format!("requires: {}\n\n", expr_to_string(expr)));
                }
            }
            if let Some(cond) = &func.ensures {
                if let Condition::Structured { expr } = cond {
                    out.push_str(&format!("ensures: {}\n\n", expr_to_string(expr)));
                }
            }
            for step in &func.steps {
                if let Step::Desc { content } = step {
                    out.push_str(&format!("  - {}\n", content.content.value));
                }
            }
            out.push('\n');
        }
        Fragment::TypeDef { typedef } => {
            out.push_str(&format!("## `type {}`\n\n", typedef.name.name));
            if let Some(d) = &typedef.desc {
                out.push_str(&format!("{}\n\n", d.content.value));
            }
            for r in &typedef.rules {
                out.push_str(&format!("rule: {}\n\n", r.desc.content.value));
            }
            if let TypeBody::Record { fields } = &typedef.body {
                for f in fields {
                    let type_str: Vec<String> = f.type_hint.iter()
                        .map(|a| atom_to_string(a))
                        .collect();
                    out.push_str(&format!("- `{}`: {}\n", f.name.name, type_str.join(" ")));
                    for r in &f.rules {
                        out.push_str(&format!("  - rule: {}\n", r.desc.content.value));
                    }
                }
                out.push('\n');
            }
        }
        Fragment::Flow { flow } => {
            out.push_str(&format!("## `flow {}`\n\n", flow.name.name));
        }
        Fragment::Ui { ui } => {
            out.push_str(&format!("## `ui {}`\n\n", ui.name.name));
        }
        _ => {}
    }
}

fn expr_to_string(expr: &mimispec::ast::Expr) -> String {
    use mimispec::ast::*;
    match expr {
        Expr::Ident { value } => value.name.clone(),
        Expr::String { value } => format!("\"{}\"", value.value),
        Expr::Number { value } => value.clone(),
        Expr::Bool { value, .. } => value.to_string(),
        Expr::List { items } => {
            let inner: Vec<String> = items.iter().map(|e| expr_to_string(e)).collect();
            format!("[{}]", inner.join(", "))
        }
        Expr::Compare { left, op, right } => {
            format!("{} {} {}", expr_to_string(left), compare_op_to_str(*op), expr_to_string(right))
        }
        Expr::And { left, right, .. } => format!("{} and {}", expr_to_string(left), expr_to_string(right)),
        Expr::Or { left, right, .. } => format!("{} or {}", expr_to_string(left), expr_to_string(right)),
        Expr::Not { expr: inner, .. } => format!("not {}", expr_to_string(inner)),
        Expr::Placeholder { .. } => "...".into(),
        _ => format!("{:?}", expr),
    }
}

fn compare_op_to_str(op: mimispec::ast::CompareOp) -> &'static str {
    use mimispec::ast::CompareOp;
    match op {
        CompareOp::Eq => "==",
        CompareOp::Ne => "!=",
        CompareOp::Lt => "<",
        CompareOp::Gt => ">",
        CompareOp::Le => "<=",
        CompareOp::Ge => ">=",
    }
}

fn atom_to_string(atom: &mimispec::ast::Atom) -> String {
    use mimispec::ast::Atom;
    match atom {
        Atom::Ident { value } => value.name.clone(),
        Atom::String { value } => format!("\"{}\"", value.value),
        Atom::Number { value } => value.clone(),
        Atom::Symbol { value } => value.clone(),
        Atom::List { items } => {
            let inner: Vec<String> = items.iter()
                .map(|group| {
                    group.iter().map(|a| atom_to_string(a)).collect::<Vec<_>>().join(", ")
                })
                .collect();
            format!("[{}]", inner.join(", "))
        }
    }
}

/// Generate normalized .mms output from a .mms source (parse + render).
pub fn generate_mms(source: &str) -> Result<String, String> {
    use mimispec::parse;
    use mimispec::render::render_file;

    let result = parse(source);
    if let Some(err) = result.errors.first() {
        return Err(format!("{:?}", err));
    }
    Ok(render_file(&result.file))
}
