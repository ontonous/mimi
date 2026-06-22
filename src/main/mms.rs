use std::fs;
use std::path::PathBuf;

use mimispec::latex::render_file_latex;
use serde::Serialize;

use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::span;
use mimi::diagnostic::Diagnostic;

#[derive(Serialize)]
struct MmsJsonError {
    line: usize,
    col: usize,
    message: String,
}

#[derive(Serialize)]
struct MmsJsonResult {
    path: String,
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ast: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    render: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latex: Option<String>,
    errors: Vec<MmsJsonError>,
}

#[derive(Serialize)]
struct MmsJsonOutput {
    results: Vec<MmsJsonResult>,
}

pub(crate) fn mms(files: &[PathBuf], show_ast: bool, json: bool, render: bool, latex: bool) -> Result<(), String> {
    let paths: Vec<PathBuf> = if files.is_empty() {
        vec![PathBuf::from("-")]
    } else {
        files.to_vec()
    };

    let mut total_errors = 0usize;
    let mut any_failure = false;
    let mut json_results = Vec::new();

    for path in &paths {
        let source = if path == &PathBuf::from("-") {
            use std::io::Read;
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).map_err(|e| format!("stdin error: {}", e))?;
            input
        } else {
            fs::read_to_string(path).map_err(|e| format!("failed to read {}: {}", path.display(), e))?
        };

        let result = mimispec::parse(&source);
        let success = result.errors.is_empty();
        let ast_value = if show_ast || json {
            serde_json::to_value(&result.file).ok()
        } else {
            None
        };
        let rendered = if render || json {
            Some(mimispec::render::render_file(&result.file))
        } else {
            None
        };
        let latex_rendered = if latex || json {
            Some(render_file_latex(&result.file))
        } else {
            None
        };

        let errors: Vec<MmsJsonError> = result.errors.iter().map(|e| MmsJsonError {
            line: e.line,
            col: e.col,
            message: e.to_string(),
        }).collect();

        let json_result = MmsJsonResult {
            path: path.display().to_string(),
            success,
            ast: ast_value,
            render: rendered,
            latex: latex_rendered,
            errors,
        };

        if !json {
            if success {
                if render && !show_ast && !latex {
                    if let Some(ref source) = json_result.render {
                        print!("{}", source);
                    }
                } else if latex && !show_ast && !render {
                    if let Some(ref latex_out) = json_result.latex {
                        println!("{}", latex_out);
                    }
                } else {
                    println!("✓ Parsing successful: {}", path.display());
                    if show_ast {
                        println!("{:#?}", result.file);
                    }
                    if render {
                        if let Some(ref source) = json_result.render {
                            println!("{}", source);
                        }
                    }
                    if latex {
                        if let Some(ref latex_out) = json_result.latex {
                            println!("{}", latex_out);
                        }
                    }
                }
            } else {
                eprintln!("✗ Parsing failed for {} with {} error(s)", path.display(), result.errors.len());
                let use_color = colors_enabled();
                let src_ref = Some(source.as_str());
                let filename = &path.display().to_string();
                for err in &result.errors {
                    let sp = span::Span::single(err.line, err.col);
                    let diag = Diagnostic::error(err.to_string(), sp);
                    let formatted = format_diagnostic(&diag, src_ref, filename);
                    if use_color {
                        eprint!("{}", formatted);
                    } else {
                        eprint!("{}", strip_ansi(&formatted));
                    }
                }
            }
        }

        if !success {
            any_failure = true;
        }
        total_errors += result.errors.len();
        json_results.push(json_result);
    }

    if json {
        let output = MmsJsonOutput { results: json_results };
        println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
    }

    if any_failure {
        if !json {
            eprintln!("\nTotal error(s): {}", total_errors);
        }
        return Err("parsing failed".into());
    }
    Ok(())
}
