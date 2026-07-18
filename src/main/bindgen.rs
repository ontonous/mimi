//! `mimi bindgen` — generate FFI bindings for all supported languages.
//!
//! Takes a `.mimi` file with `extern "C"` declarations and generates
//! binding code for C, C++, Rust, Go, Node.js, Python, and Java.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use mimi::ast;
use mimi::ffi;

pub(crate) fn run(path: &Path, output_dir: &Path) -> Result<(), String> {
    let source = mimi::path_safety::read_source_capped(path)?;
    let tokens = mimi::lexer::Lexer::new(&source).tokenize()?;
    let file = mimi::parser::Parser::new(tokens).parse_file()?;
    let checked = crate::emit::checked_component_input(&file)?;

    let mut extern_funcs = crate::emit::resolved_extern_funcs(&checked)?;
    let mut exported_funcs = Vec::new();
    let mut type_defs = HashMap::new();
    collect_exported_and_types(&file, &mut exported_funcs, &mut type_defs);

    if extern_funcs.is_empty() && exported_funcs.is_empty() {
        return Err("no extern or exported functions found in the file".to_string());
    }

    // Include exported functions as extern-like declarations
    for ef in &exported_funcs {
        let extern_func = ast::ExternFunc {
            name: ef.name.clone(),
            params: ef
                .params
                .iter()
                .map(|p| ast::ExternParam {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    cap_mode: None,
                })
                .collect(),
            ret: ef.ret.clone(),
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
        };
        extern_funcs.push(extern_func);
    }

    let pkg_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mimi_module")
        .to_string();

    // Create output directory
    fs::create_dir_all(output_dir)
        .map_err(|e| format!("failed to create {}: {}", output_dir.display(), e))?;

    println!("=== mimi bindgen ===");
    println!("Source: {}", path.display());
    println!("Output: {}", output_dir.display());
    println!("Functions: {}", extern_funcs.len());
    println!();

    // Generate C header
    let c_header = ffi::c_header::generate_c_header(&extern_funcs, type_defs.clone())
        .map_err(|e| format!("C header generation failed: {}", e))?;
    let c_path = output_dir.join(format!("{}.h", pkg_name));
    fs::write(&c_path, &c_header)
        .map_err(|e| format!("failed to write {}: {}", c_path.display(), e))?;
    println!("  [C]        {}", c_path.display());

    // Generate C++ bindings
    let cpp_gen = ffi::cpp_bind::CppBindGenerator::new(type_defs.clone(), &pkg_name);
    let cpp_header = cpp_gen
        .generate(&extern_funcs)
        .map_err(|e| format!("C++ generation failed: {}", e))?;
    let cpp_path = output_dir.join(format!("{}.hpp", pkg_name));
    fs::write(&cpp_path, &cpp_header)
        .map_err(|e| format!("failed to write {}: {}", cpp_path.display(), e))?;
    println!("  [C++]      {}", cpp_path.display());

    // Generate Rust bindings
    let rust_gen = ffi::rust_bind::RustBindGenerator::new(type_defs.clone(), &pkg_name);
    let rust_code = rust_gen
        .generate(&extern_funcs)
        .map_err(|e| format!("Rust generation failed: {}", e))?;
    let rust_path = output_dir.join(format!("{}.rs", pkg_name));
    fs::write(&rust_path, &rust_code)
        .map_err(|e| format!("failed to write {}: {}", rust_path.display(), e))?;
    println!("  [Rust]     {}", rust_path.display());

    // Generate Go bindings
    let go_gen = ffi::go_bind::GoBindGenerator::new(type_defs.clone(), &pkg_name);
    let go_code = go_gen
        .generate(&extern_funcs)
        .map_err(|e| format!("Go generation failed: {}", e))?;
    let go_path = output_dir.join(format!("{}.go", pkg_name));
    fs::write(&go_path, &go_code)
        .map_err(|e| format!("failed to write {}: {}", go_path.display(), e))?;
    println!("  [Go]       {}", go_path.display());

    // Generate Node.js bindings
    let node_gen = ffi::node_bind::NodeBindGenerator::new(type_defs.clone(), &pkg_name);
    let node_code = node_gen
        .generate(&extern_funcs)
        .map_err(|e| format!("Node.js generation failed: {}", e))?;
    let node_path = output_dir.join(format!("{}_napi.c", pkg_name));
    fs::write(&node_path, &node_code)
        .map_err(|e| format!("failed to write {}: {}", node_path.display(), e))?;
    println!("  [Node.js]  {}", node_path.display());

    // Generate TypeScript declarations
    let ts_code = node_gen
        .generate_dts(&extern_funcs)
        .map_err(|e| format!("TypeScript generation failed: {}", e))?;
    let ts_path = output_dir.join(format!("{}.d.ts", pkg_name));
    fs::write(&ts_path, &ts_code)
        .map_err(|e| format!("failed to write {}: {}", ts_path.display(), e))?;
    println!("  [Typescript] {}", ts_path.display());

    // Generate Python bindings
    let py_gen = ffi::py_bind::PyBindGenerator::new(type_defs.clone(), &pkg_name);
    let py_cpp = py_gen
        .generate(&extern_funcs)
        .map_err(|e| format!("Python binding generation failed: {}", e))?;
    let py_path = output_dir.join(format!("{}_pybind.cpp", pkg_name));
    fs::write(&py_path, &py_cpp)
        .map_err(|e| format!("failed to write {}: {}", py_path.display(), e))?;
    println!("  [Python]   {}", py_path.display());

    let pyi = py_gen
        .generate_pyi(&extern_funcs)
        .map_err(|e| format!("Python stub generation failed: {}", e))?;
    let pyi_path = output_dir.join(format!("{}.pyi", pkg_name));
    fs::write(&pyi_path, &pyi)
        .map_err(|e| format!("failed to write {}: {}", pyi_path.display(), e))?;
    println!("  [Python stub] {}", pyi_path.display());

    // Generate Java bindings
    let java_gen = ffi::jni_bind::JniBindGenerator::new(type_defs.clone(), &pkg_name);
    let java_c = java_gen
        .generate_c(&extern_funcs)
        .map_err(|e| format!("Java JNI generation failed: {}", e))?;
    let java_c_path = output_dir.join(format!("{}_jni.c", pkg_name));
    fs::write(&java_c_path, &java_c)
        .map_err(|e| format!("failed to write {}: {}", java_c_path.display(), e))?;
    println!("  [Java/JNI] {}", java_c_path.display());

    let java_class = java_gen
        .generate_java(&extern_funcs)
        .map_err(|e| format!("Java class generation failed: {}", e))?;
    let java_path = output_dir.join(format!("{}.java", capitalize(&pkg_name)));
    fs::write(&java_path, &java_class)
        .map_err(|e| format!("failed to write {}: {}", java_path.display(), e))?;
    println!("  [Java]     {}", java_path.display());

    println!();
    println!(
        "Generated 9 binding files for {} functions.",
        extern_funcs.len()
    );

    Ok(())
}

fn collect_exported_and_types(
    file: &ast::File,
    exported_funcs: &mut Vec<ast::FuncDef>,
    type_defs: &mut HashMap<String, ast::TypeDef>,
) {
    for item in &file.items {
        match item {
            ast::Item::Func(f) => {
                if f.extern_abi.is_some() {
                    exported_funcs.push(f.clone());
                }
            }
            ast::Item::Type(t) => {
                type_defs.insert(t.name.clone(), t.clone());
            }
            ast::Item::Module(m) => {
                collect_exported_and_types(
                    &ast::File {
                        imports: Vec::new(),
                        items: m.items.clone(),
                        implicit_single: false,
                    },
                    exported_funcs,
                    type_defs,
                );
            }
            _ => {}
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}
