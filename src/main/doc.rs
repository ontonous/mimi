use std::fs;
use std::path::Path;

pub(crate) fn doc(path: &Path, format: &str, output: Option<&Path>) -> Result<(), String> {
    let source = mimi::path_safety::read_source_capped(path)?;

    let is_mms = path.extension().map(|e| e == "mms").unwrap_or(false);

    let doc_text = match format {
        "markdown" | "md" => {
            if is_mms {
                mimi::doc_core::generate_markdown_from_mms(&source)?
            } else {
                mimi::doc_core::generate_markdown(&source)?
            }
        }
        "mms" => {
            if !is_mms {
                return Err("mms output format requires .mms input".into());
            }
            mimi::doc_core::generate_mms(&source)?
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
