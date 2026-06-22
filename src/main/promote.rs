use std::fs;
use std::path::Path;

pub(crate) fn promote(path: &Path, output: Option<&Path>) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;

    // Check for ... placeholders
    if source.contains("...") {
        return Err(format!("file contains '...' placeholders, cannot promote: {}", path.display()));
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
