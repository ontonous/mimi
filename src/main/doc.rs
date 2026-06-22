use std::fs;
use std::path::Path;

pub(crate) fn doc(path: &Path, format: &str, output: Option<&Path>) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;

    let doc_text = match format {
        "markdown" | "md" => mimi::doc_core::generate_markdown(&source)?,
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
