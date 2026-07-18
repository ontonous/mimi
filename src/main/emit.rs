use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::resolve_path;
use mimi::ast::{self, File};
use mimi::core::BackendProfile;
use mimi::{ffi, lexer, parser};

pub(crate) fn checked_component_input<'a>(
    file: &'a File,
) -> Result<mimi::core::CheckedProgram<'a>, String> {
    let checked = mimi::core::check_program(file).map_err(|diagnostics| {
        let messages = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        format!("component input failed type checking: {messages}")
    })?;
    checked
        .validate_backend(BackendProfile::Component)
        .map_err(|diagnostics| {
            let messages = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            format!("component backend rejected input: {messages}")
        })?;
    Ok(checked)
}

pub(crate) fn resolved_extern_funcs(
    checked: &mimi::core::CheckedProgram<'_>,
) -> Result<Vec<ast::ExternFunc>, String> {
    let mut symbols = std::collections::HashSet::new();
    let mut blocks = checked.extern_blocks().values().collect::<Vec<_>>();
    blocks.sort_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
    let mut funcs = Vec::new();
    for block in blocks {
        for signature in &block.signatures {
            if !symbols.insert(signature.name.clone()) {
                return Err(format!(
                    "component extern symbol '{}' is declared more than once",
                    signature.name
                ));
            }
            funcs.push(ast::ExternFunc {
                name: signature.name.clone(),
                params: signature
                    .typed_params
                    .iter()
                    .map(|(name, ty, cap_mode)| ast::ExternParam {
                        name: name.clone(),
                        ty: ty.clone(),
                        cap_mode: *cap_mode,
                    })
                    .collect(),
                ret: signature.ret_type.clone(),
                requires: signature.requires.clone(),
                ensures: signature.ensures.clone(),
                variadic: signature.variadic,
                no_panic: signature.no_panic || block.no_panic,
            });
        }
    }
    Ok(funcs)
}

pub(crate) fn resolved_exported_funcs(
    checked: &mimi::core::CheckedProgram<'_>,
    extern_funcs: &[ast::ExternFunc],
) -> Result<Vec<ast::FuncDef>, String> {
    let mut symbols = extern_funcs
        .iter()
        .map(|func| func.name.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut functions = checked.functions().values().collect::<Vec<_>>();
    functions.sort_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
    let mut exported = Vec::new();
    for function in functions {
        let Some(abi) = &function.extern_abi else {
            continue;
        };
        if function.is_async || !function.generics.is_empty() || !function.where_clause.is_empty() {
            return Err(format!(
                "component export '{}' uses unsupported async/generic declaration",
                function.qualified_name
            ));
        }
        let symbol = function
            .qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(&function.qualified_name)
            .to_string();
        if !symbols.insert(symbol.clone()) {
            return Err(format!(
                "component symbol '{}' is declared more than once",
                symbol
            ));
        }
        exported.push(ast::FuncDef {
            name: symbol,
            pub_: function.pub_,
            params: function.param_decls.clone(),
            ret: Some(function.ret.clone()),
            body: Vec::new(),
            where_clause: function.where_clause.clone(),
            generics: function.generics.clone(),
            effects: function.effects.clone(),
            is_comptime: function.is_comptime,
            is_async: function.is_async,
            extern_abi: Some(abi.clone()),
            pos: {
                let span = function.origin.user_span();
                (span.start_line, span.start_col)
            },
        });
    }
    Ok(exported)
}

pub(crate) fn resolved_type_defs(
    checked: &mimi::core::CheckedProgram<'_>,
) -> Result<HashMap<String, ast::TypeDef>, String> {
    let mut definitions = checked.type_defs().values().collect::<Vec<_>>();
    definitions.sort_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
    let mut projected = HashMap::new();
    for definition in definitions {
        if definition.declaration.decl_pos.is_none() {
            continue;
        }
        let name = definition
            .qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(&definition.qualified_name)
            .to_string();
        let mut declaration = definition.declaration.clone();
        declaration.name = name.clone();
        if projected.insert(name.clone(), declaration).is_some() {
            return Err(format!(
                "component type name '{}' is declared more than once after projection",
                name
            ));
        }
    }
    Ok(projected)
}

#[cfg(test)]
mod tests {
    use super::{
        checked_component_input, resolved_exported_funcs, resolved_extern_funcs, resolved_type_defs,
    };

    fn parse(source: &str) -> mimi::ast::File {
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse")
    }

    #[test]
    fn component_input_rejects_type_errors_before_generation() {
        let file = parse(
            r#"
extern "C" {
    func bad(x: MissingType) -> i32
}
"#,
        );
        let error = checked_component_input(&file).expect_err("must reject unresolved type");
        assert!(error.contains("type checking"));
    }

    #[test]
    fn component_input_rejects_unsupported_flow_capabilities() {
        let file = parse(
            r#"
flow Choice {
    state Pending
    state Yes
    state No
    transition decide(Pending) -> Yes | No { do { return Yes {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let error = checked_component_input(&file).expect_err("component must reject multi-target");
        assert!(error.contains("flow.multi_target"));
    }

    #[test]
    fn resolved_extern_catalog_preserves_checked_signature_metadata() {
        let file = parse(
            r#"
#[no_panic]
extern "C" {
    func read(&buf: c_borrow u8) -> i32
}
"#,
        );
        let checked = checked_component_input(&file).expect("checked component");
        let funcs = resolved_extern_funcs(&checked).expect("resolved extern catalog");
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "read");
        assert_eq!(funcs[0].params[0].name, "buf");
        assert_eq!(
            funcs[0].params[0].cap_mode,
            Some(mimi::ast::CapMode::Borrow)
        );
        assert!(funcs[0].no_panic);
    }

    #[test]
    fn resolved_extern_catalog_rejects_duplicate_projected_symbols() {
        let file = parse(
            r#"
module a { extern "C" { func collide(x: i32) -> i32 } }
module b { extern "C" { func collide(x: i32) -> i32 } }
"#,
        );
        let checked = checked_component_input(&file).expect("checked component");
        let error = resolved_extern_funcs(&checked).expect_err("duplicate symbols must fail");
        assert!(error.contains("collide"));
    }

    #[test]
    fn resolved_export_catalog_preserves_checked_signature() {
        let file = parse(
            r#"
extern "C" func exported(x: i32) -> i32 { x }
"#,
        );
        let checked = checked_component_input(&file).expect("checked component");
        let externs = resolved_extern_funcs(&checked).expect("extern catalog");
        let exported = resolved_exported_funcs(&checked, &externs).expect("export catalog");
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].name, "exported");
        assert_eq!(exported[0].params[0].name, "x");
        assert_eq!(exported[0].extern_abi.as_deref(), Some("C"));
    }

    #[test]
    fn resolved_type_catalog_preserves_layout_and_rejects_projection_collisions() {
        let file = parse(
            r#"
#[repr(C)]
type Point { x: i32, y: i32 }
"#,
        );
        let checked = checked_component_input(&file).expect("checked component");
        let types = resolved_type_defs(&checked).expect("resolved type catalog");
        let point = types.get("Point").expect("Point");
        assert!(point.attributes.contains(&mimi::ast::TypeAttribute::ReprC));

        let colliding = parse(
            r#"
module a { type Point { x: i32 } }
module b { type Point { y: i32 } }
"#,
        );
        let error =
            checked_component_input(&colliding).expect_err("duplicate type names must fail");
        assert!(error.contains("Point"));
    }
}

pub(crate) fn emit_c_headers(path: Option<&Path>, output: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;
    let checked = checked_component_input(&file)?;

    let extern_funcs = resolved_extern_funcs(&checked)?;
    let exported_funcs = resolved_exported_funcs(&checked, &extern_funcs)?;
    let type_defs = resolved_type_defs(&checked)?;

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

pub(crate) fn emit_py_bindings(
    path: Option<&Path>,
    output: Option<&Path>,
    mimi_lib: Option<&Path>,
) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;
    let checked = checked_component_input(&file)?;

    let mut extern_funcs = resolved_extern_funcs(&checked)?;
    let exported_funcs = resolved_exported_funcs(&checked, &extern_funcs)?;
    let type_defs = resolved_type_defs(&checked)?;
    // Also include exported functions as extern-like declarations for Python bindings
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

    let gen = ffi::py_bind::PyBindGenerator::new(type_defs, &pkg_name);
    let bindings = gen
        .generate(&extern_funcs)
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
            let mimi_lib_str = mimi_lib
                .map(|p| p.display().to_string())
                .unwrap_or_default();
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

pub(crate) fn emit_rust_bindings(path: Option<&Path>, output: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;
    let checked = checked_component_input(&file)?;

    let extern_funcs = resolved_extern_funcs(&checked)?;
    let _exported_funcs = resolved_exported_funcs(&checked, &extern_funcs)?;
    let type_defs = resolved_type_defs(&checked)?;

    let pkg_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mimi_module")
        .to_string();
    let gen = ffi::rust_bind::RustBindGenerator::new(type_defs, &pkg_name);
    let bindings = gen
        .generate(&extern_funcs)
        .map_err(|e| format!("failed to generate Rust bindings: {}", e))?;

    match output {
        Some(out_path) => {
            fs::write(out_path, &bindings)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
            println!("Generated Rust bindings: {}", out_path.display());
        }
        None => println!("{}", bindings),
    }
    Ok(())
}

pub(crate) fn emit_go_bindings(path: Option<&Path>, output: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;
    let checked = checked_component_input(&file)?;

    let extern_funcs = resolved_extern_funcs(&checked)?;
    let type_defs = resolved_type_defs(&checked)?;

    let pkg_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mimi_module")
        .to_string();
    let gen = ffi::go_bind::GoBindGenerator::new(type_defs, &pkg_name);
    let bindings = gen
        .generate(&extern_funcs)
        .map_err(|e| format!("failed to generate Go bindings: {}", e))?;

    match output {
        Some(out_path) => {
            fs::write(out_path, &bindings)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
            println!("Generated Go bindings: {}", out_path.display());
        }
        None => println!("{}", bindings),
    }
    Ok(())
}

pub(crate) fn emit_node_bindings(
    path: Option<&Path>,
    output: Option<&Path>,
    ts_output: Option<&Path>,
) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;
    let checked = checked_component_input(&file)?;

    let extern_funcs = resolved_extern_funcs(&checked)?;
    let type_defs = resolved_type_defs(&checked)?;

    let pkg_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mimi_module")
        .to_string();
    let gen = ffi::node_bind::NodeBindGenerator::new(type_defs, &pkg_name);
    let bindings = gen
        .generate(&extern_funcs)
        .map_err(|e| format!("failed to generate Node.js bindings: {}", e))?;

    match output {
        Some(out_path) => {
            fs::write(out_path, &bindings)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
            println!("Generated Node.js N-API bindings: {}", out_path.display());
        }
        None => println!("{}", bindings),
    }

    // Generate TypeScript declarations
    if let Some(ts_path) = ts_output {
        let dts = gen
            .generate_dts(&extern_funcs)
            .map_err(|e| format!("failed to generate TypeScript declarations: {}", e))?;
        fs::write(ts_path, &dts)
            .map_err(|e| format!("failed to write {}: {}", ts_path.display(), e))?;
        println!("Generated TypeScript declarations: {}", ts_path.display());
    }
    Ok(())
}

pub(crate) fn emit_java_bindings(
    path: Option<&Path>,
    output: Option<&Path>,
    java_output: Option<&Path>,
) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;
    let checked = checked_component_input(&file)?;

    let extern_funcs = resolved_extern_funcs(&checked)?;
    let type_defs = resolved_type_defs(&checked)?;

    let pkg_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mimi_module")
        .to_string();
    let gen = ffi::jni_bind::JniBindGenerator::new(type_defs, &pkg_name);
    let c_bridge = gen
        .generate_c(&extern_funcs)
        .map_err(|e| format!("failed to generate JNI C bridge: {}", e))?;

    match output {
        Some(out_path) => {
            fs::write(out_path, &c_bridge)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
            println!("Generated JNI C bridge: {}", out_path.display());
        }
        None => println!("{}", c_bridge),
    }

    // Generate Java interface class
    if let Some(java_path) = java_output {
        let java_class = gen
            .generate_java(&extern_funcs)
            .map_err(|e| format!("failed to generate Java class: {}", e))?;
        fs::write(java_path, &java_class)
            .map_err(|e| format!("failed to write {}: {}", java_path.display(), e))?;
        println!("Generated Java class: {}", java_path.display());
    }
    Ok(())
}

pub(crate) fn emit_cpp_bindings(path: Option<&Path>, output: Option<&Path>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;
    let checked = checked_component_input(&file)?;

    let extern_funcs = resolved_extern_funcs(&checked)?;
    let type_defs = resolved_type_defs(&checked)?;

    let pkg_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mimi_module")
        .to_string();
    let gen = ffi::cpp_bind::CppBindGenerator::new(type_defs, &pkg_name);
    let bindings = gen
        .generate(&extern_funcs)
        .map_err(|e| format!("failed to generate C++ bindings: {}", e))?;

    match output {
        Some(out_path) => {
            fs::write(out_path, &bindings)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
            println!("Generated C++ bindings: {}", out_path.display());
        }
        None => println!("{}", bindings),
    }
    Ok(())
}
