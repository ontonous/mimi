use std::fs;
use std::path::Path;

pub(crate) fn doc(path: &Path, format: &str, output: Option<&Path>) -> Result<(), String> {
    if path.extension().map(|e| e == "mms").unwrap_or(false) {
        return Err(
            "MimiSpec (.mms) support is removed in 0.1.8; promote sketches to .mimi first".into(),
        );
    }

    let source = mimi::path_safety::read_source_capped(path)?;

    let doc_text = match format {
        "markdown" | "md" => mimi::doc_core::generate_markdown(&source)?,
        "mms" => {
            return Err(
                "MimiSpec (.mms) output is removed in 0.1.8; use Markdown from .mimi".into(),
            );
        }
        _ => return Err(format!("unsupported doc format: {}", format)),
    };

    match output {
        Some(out_path) => {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create output dir: {}", e))?;
            }
            fs::write(out_path, &doc_text)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
        }
        None => {
            print!("{}", doc_text);
        }
    }

    Ok(())
}
