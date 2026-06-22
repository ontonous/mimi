use std::collections::HashMap;
use std::fs;
use std::path::Path;

use mimi::ast::{self, File, Item};
use mimi::{ffi, lexer, parser};
use crate::resolve_path;

pub(crate) fn emit_c_headers(path: Option<&Path>, output: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    let mut extern_funcs = Vec::new();
    let mut exported_funcs = Vec::new();
    let mut type_defs = HashMap::new();
    collect_extern_and_types(&file, &mut extern_funcs, &mut type_defs);
    collect_exported_and_types(&file, &mut exported_funcs, &mut type_defs);

    let header = if exported_funcs.is_empty() {
        ffi::c_header::generate_c_header(&extern_funcs, type_defs)?
    } else {
        ffi::c_header::generate_c_header_with_exported(&extern_funcs, &exported_funcs, type_defs)?
    };

    match output {
        Some(out_path) => {
            fs::write(out_path, &header)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
            println!("✓ Generated C header: {}", out_path.display());
        }
        None => {
            println!("{}", header);
        }
    }
    Ok(())
}

pub(crate) fn emit_py_bindings(path: Option<&Path>, output: Option<&Path>, mimi_lib: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    let mut extern_funcs = Vec::new();
    let mut exported_funcs = Vec::new();
    let mut type_defs = HashMap::new();
    collect_extern_and_types(&file, &mut extern_funcs, &mut type_defs);
    collect_exported_and_types(&file, &mut exported_funcs, &mut type_defs);
    // Also include exported functions as extern-like declarations for Python bindings
    for ef in &exported_funcs {
        let extern_func = ast::ExternFunc {
            name: ef.name.clone(),
            params: ef.params.iter().map(|p| ast::ExternParam {
                name: p.name.clone(),
                ty: p.ty.clone(),
                cap_mode: None,
            }).collect(),
            ret: ef.ret.clone(),
            requires: None,
            ensures: None,
            variadic: false,
        };
        extern_funcs.push(extern_func);
    }

    let pkg_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mimi_module")
        .to_string();

    let gen = ffi::py_bind::PyBindGenerator::new(type_defs, &pkg_name);
    let bindings = gen.generate(&extern_funcs)
        .map_err(|e| format!("failed to generate bindings: {}", e))?;

    match output {
        Some(out_path) => {
            fs::write(out_path, &bindings)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
            println!("✓ Generated Python bindings: {}", out_path.display());
            // Emit .pyi type stub next to the .cpp
            let pyi_out = out_path.with_extension("pyi");
            if let Ok(pyi) = gen.generate_pyi(&extern_funcs) {
                fs::write(&pyi_out, &pyi)
                    .map_err(|e| format!("failed to write {}: {}", pyi_out.display(), e))?;
                println!("✓ Generated Python type stubs: {}", pyi_out.display());
            }
            // Also emit a CMakeLists.txt next to the output
            let cmake_out = out_path.with_extension("cmake");
            let mimi_lib_str = mimi_lib.map(|p| p.display().to_string()).unwrap_or_default();
            let cmake = ffi::py_bind::generate_cmake_snippet(
                &pkg_name,
                "./",
                "/usr/local/lib",
                &mimi_lib_str,
            );
            fs::write(&cmake_out, cmake)
                .map_err(|e| format!("failed to write {}: {}", cmake_out.display(), e))?;
            println!("✓ Generated CMakeLists.txt: {}", cmake_out.display());
        }
        None => {
            println!("{}", bindings);
        }
    }
    Ok(())
}

fn collect_extern_and_types(
    file: &File,
    extern_funcs: &mut Vec<ast::ExternFunc>,
    type_defs: &mut HashMap<String, ast::TypeDef>,
) {
    for item in &file.items {
        match item {
            Item::ExternBlock(block) => {
                extern_funcs.extend(block.funcs.iter().cloned());
            }
            Item::Type(t) => {
                type_defs.insert(t.name.clone(), t.clone());
            }
            Item::Module(m) => {
                collect_extern_and_types(
                    &ast::File {
                        imports: Vec::new(),
                        items: m.items.clone(),
                    },
                    extern_funcs,
                    type_defs,
                );
            }
            _ => {}
        }
    }
}

/// Collect Mimi→C exported functions (marked `extern "C" func`) and type defs.
fn collect_exported_and_types(
    file: &File,
    exported_funcs: &mut Vec<ast::FuncDef>,
    type_defs: &mut HashMap<String, ast::TypeDef>,
) {
    for item in &file.items {
        match item {
            Item::Func(f) => {
                if f.extern_abi.is_some() {
                    exported_funcs.push(f.clone());
                }
            }
            Item::Type(t) => {
                type_defs.insert(t.name.clone(), t.clone());
            }
            Item::Module(m) => {
                collect_exported_and_types(
                    &ast::File {
                        imports: Vec::new(),
                        items: m.items.clone(),
                    },
                    exported_funcs,
                    type_defs,
                );
            }
            _ => {}
        }
    }
}
