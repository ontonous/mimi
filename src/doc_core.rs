use crate::ast::{Item, Type, TypeDefKind};
use crate::{lexer, parser};

fn item_line(item: &Item) -> usize {
    match item {
        Item::Func(f) => f.meta.span.start_line,
        _ => 0,
    }
}

/// Extract comment lines immediately preceding an item definition.
fn extract_preceding_comments(source: &str, item: &Item) -> String {
    let item_line = item_line(item);
    if item_line == 0 {
        return String::new();
    }
    let lines: Vec<&str> = source.lines().collect();
    let idx = item_line.saturating_sub(1); // 0-indexed
    let mut block = Vec::new();
    let mut i = idx.wrapping_sub(1);
    while i < lines.len() && lines[i].trim_start().starts_with("//") {
        let text = lines[i].trim_start().trim_start_matches('/').trim();
        block.push(text.to_string());
        i = i.wrapping_sub(1);
    }
    block.reverse();
    block.join("\n")
}

fn type_to_string(ty: &Type) -> String {
    match ty {
        Type::Located { ty, .. } => type_to_string(ty),
        Type::Name(name, generics) => {
            if generics.is_empty() {
                name.clone()
            } else {
                let args: Vec<String> = generics.iter().map(type_to_string).collect();
                format!("{}<{}>", name, args.join(", "))
            }
        }
        Type::Ref(lifetime, inner) => {
            let lt = lifetime
                .as_ref()
                .map(|l| format!("'{} ", l))
                .unwrap_or_default();
            format!("&{}{}", lt, type_to_string(inner))
        }
        Type::RefMut(lifetime, inner) => {
            let lt = lifetime
                .as_ref()
                .map(|l| format!("'{} ", l))
                .unwrap_or_default();
            format!("&{}mut {}", lt, type_to_string(inner))
        }
        Type::Option(inner) => format!("Option<{}>", type_to_string(inner)),
        Type::Result(ok, err) => format!("Result<{}, {}>", type_to_string(ok), type_to_string(err)),
        Type::Tuple(types) => {
            let inner: Vec<String> = types.iter().map(type_to_string).collect();
            format!("({})", inner.join(", "))
        }
        Type::Func(args, ret) => {
            let args: Vec<String> = args.iter().map(type_to_string).collect();
            format!("fn({}) -> {}", args.join(", "), type_to_string(ret))
        }
        Type::ExternFunc(args, ret) => {
            let args: Vec<String> = args.iter().map(type_to_string).collect();
            format!(
                "extern \"C\" fn({}) -> {}",
                args.join(", "),
                type_to_string(ret)
            )
        }
        Type::CBuffer(inner) => format!("c_buffer<{}>", type_to_string(inner)),
        Type::Cap(name) => format!("cap {}", name),
        Type::CapAtom(name) => format!("cap {}", name),
        Type::Shared(inner) => format!("shared<{}>", type_to_string(inner)),
        Type::Weak(inner) => format!("weak<{}>", type_to_string(inner)),
        Type::Newtype(name, inner) => format!("{} /* newtype({}) */", name, type_to_string(inner)),
        Type::Nothing => "nothing".to_string(),
        Type::TyErr => "«error»".to_string(),
        Type::Array(inner, size) => format!("[{}; {}]", type_to_string(inner), size),
        Type::Slice(inner) => format!("&[{}]", type_to_string(inner)),
        Type::ImplTrait(traits) => format!("impl {}", traits.join(" + ")),
        Type::DynTrait(traits) => format!("dyn {}", traits.join(" + ")),
        Type::RawPtr(inner) => format!("*{}", type_to_string(inner)),
        Type::RawPtrMut(inner) => format!("*mut {}", type_to_string(inner)),
        Type::Infer => "_".to_string(),
        Type::TypeVar(id) => format!("?T{}", id),
        Type::ForAll(params, body) => {
            format!("forall {}. {}", params.join(", "), type_to_string(body))
        }
    }
}

/// Generate Markdown from a .mimi source (Mimi parser).
pub fn generate_markdown(source: &str) -> Result<String, String> {
    let tokens = lexer::Lexer::new(source).tokenize()?;
    let file = parser::Parser::new_memory(tokens, "doc_core", "markdown", source)
        .map_err(|error| error.to_string())?
        .parse_file()?;

    let mut out = String::new();

    for item in &file.items {
        let comment = extract_preceding_comments(source, item);
        if !comment.is_empty() {
            for line in comment.lines() {
                out.push_str(&format!("> *{}*\n\n", line));
            }
        }
        match item {
            Item::Func(f) => {
                let params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, type_to_string(&p.ty)))
                    .collect();
                let ret = f
                    .ret
                    .as_ref()
                    .map(|t| format!(" -> {}", type_to_string(t)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "## `func {}({}){}`\n\n",
                    f.name,
                    params.join(", "),
                    ret
                ));
                // 0.35.13 trivia-ization: desc:/rule: no longer reach the
                // AST (parser consumes them as trivia) — nothing to extract.
            }
            Item::Type(t) => {
                out.push_str(&format!("## `type {}`\n\n", t.name));
                match &t.kind {
                    TypeDefKind::Record(fields) => {
                        for field in fields {
                            out.push_str(&format!(
                                "- `{}`: {}\n",
                                field.name,
                                type_to_string(&field.ty)
                            ));
                        }
                        out.push('\n');
                    }
                    TypeDefKind::Enum(variants) => {
                        for v in variants {
                            match &v.payload {
                                Some(crate::ast::VariantPayload::Tuple(types)) => {
                                    let inner: Vec<String> =
                                        types.iter().map(type_to_string).collect();
                                    out.push_str(&format!(
                                        "- `{}({})`\n",
                                        v.name,
                                        inner.join(", ")
                                    ));
                                }
                                Some(crate::ast::VariantPayload::Record(fields)) => {
                                    out.push_str(&format!("- `{}`:\n", v.name));
                                    for f in fields {
                                        out.push_str(&format!(
                                            "  - `{}`: {}\n",
                                            f.name,
                                            type_to_string(&f.ty)
                                        ));
                                    }
                                }
                                None => {
                                    out.push_str(&format!("- `{}`\n", v.name));
                                }
                            }
                        }
                        out.push('\n');
                    }
                    TypeDefKind::Alias(inner) => {
                        out.push_str(&format!("alias for `{}`\n\n", type_to_string(inner)));
                    }
                    TypeDefKind::Newtype(inner) => {
                        out.push_str(&format!("newtype over `{}`\n\n", type_to_string(inner)));
                    }
                    TypeDefKind::Union(fields) => {
                        out.push_str("union:\n");
                        for field in fields {
                            out.push_str(&format!(
                                "- `{}`: {}\n",
                                field.name,
                                type_to_string(&field.ty)
                            ));
                        }
                        out.push('\n');
                    }
                }
            }
            Item::Module(m) => {
                out.push_str(&format!("## `module {}`\n\n", m.name));
                for sub_item in &m.items {
                    // Render nested items inline
                    match sub_item {
                        Item::Func(f) => {
                            let params: Vec<String> = f
                                .params
                                .iter()
                                .map(|p| format!("{}: {}", p.name, type_to_string(&p.ty)))
                                .collect();
                            let ret = f
                                .ret
                                .as_ref()
                                .map(|t| format!(" -> {}", type_to_string(t)))
                                .unwrap_or_default();
                            out.push_str(&format!(
                                "### `func {}({}){}`\n\n",
                                f.name,
                                params.join(", "),
                                ret
                            ));
                            // 0.35.13 trivia-ization: desc:/rule: are trivia.
                        }
                        Item::Type(t) => {
                            out.push_str(&format!("### `type {}`\n\n", t.name));
                            if let TypeDefKind::Record(fields) = &t.kind {
                                for field in fields {
                                    out.push_str(&format!(
                                        "- `{}`: {}\n",
                                        field.name,
                                        type_to_string(&field.ty)
                                    ));
                                }
                                out.push('\n');
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(out)
}

/// [removed] 0.1.8 Phase E: `.mms` support and the external `mimispec`
/// parser are no longer part of the compiler. This stub keeps the CLI's
/// `doc` command able to explain the removal instead of crashing.
pub fn generate_markdown_from_mms(_source: &str) -> Result<String, String> {
    Err("MimiSpec (.mms) support is removed in 0.1.8; promote sketches to .mimi first".to_string())
}

/// [removed] See `generate_markdown_from_mms`.
pub fn generate_mms(_source: &str) -> Result<String, String> {
    Err("MimiSpec (.mms) output is removed in 0.1.8; use Markdown from .mimi".to_string())
}

#[cfg(test)]
mod tests {
    use super::generate_markdown;

    #[test]
    fn markdown_uses_registered_declaration_metadata_for_comments() {
        let source = "// Adds one.\nfunc add_one(value: i32) -> i32 { value + 1 }\n";
        let markdown = generate_markdown(source).expect("generate Mimi markdown");

        assert!(markdown.contains("> *Adds one.*"));
        assert!(markdown.contains("## `func add_one(value: i32) -> i32`"));
    }
}
