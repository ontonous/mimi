//! `mimi abi` — Component IR `.mimiabi` export/validation/diff CLI.
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use mimi::component::{
    diff_abi, mimi_type_to_abi, register_core_runtime_abi, AbiAlias, AbiEnum, AbiField,
    AbiGenerator, AbiPrimitive, AbiStruct, AbiTypeDef, ComponentIdentity, MimiAbi, MimiAbiError,
};
use mimi::{ast, ffi, lexer};

use super::AbiAction;
use crate::abi_bridge;

pub(crate) fn run(action: AbiAction) -> Result<(), String> {
    match action {
        AbiAction::Core { output } => {
            let mut gen = AbiGenerator::new();
            register_core_runtime_abi(&mut gen);
            let ir = gen.build();
            let abi = MimiAbi::from_component_ir(&ir);
            write_json(output.as_deref(), &abi)
        }
        AbiAction::Validate { input } => {
            let abi = read_abi(&input)?;
            println!(
                "valid .mimiabi: identity={}:{}, exports={}, imports={}, types={}",
                abi.identity.name,
                abi.identity.version,
                abi.exports.len(),
                abi.imports.len(),
                abi.types.len()
            );
            Ok(())
        }
        AbiAction::Hash { input } => {
            let abi = read_abi(&input)?;
            let hash = abi.hash().map_err(|e| format!("hash failed: {e}"))?;
            println!("{hash}");
            Ok(())
        }
        AbiAction::Diff { left, right } => {
            let old = read_abi(&left)?;
            let new = read_abi(&right)?;
            let diff = diff_abi(&old, &new);
            for change in &diff.changes {
                println!("{change}");
            }
            println!("summary: {}", diff.summary());
            Ok(())
        }
        AbiAction::EmitC { input, output } => {
            let abi = read_abi(&input)?;
            let ir = abi.to_component_ir();
            let code = mimi::component::generate_c_header(&ir);
            write_text(output.as_deref(), &code)
        }
        AbiAction::EmitRust { input, output } => {
            let abi = read_abi(&input)?;
            let ir = abi.to_component_ir();
            let code = mimi::component::generate_rust_bindings(&ir);
            write_text(output.as_deref(), &code)
        }
        AbiAction::Check { input } => {
            let mut gen = AbiGenerator::new();
            register_core_runtime_abi(&mut gen);
            let current = MimiAbi::from_component_ir(&gen.build());
            let candidate = read_abi(&input)?;
            let diff = diff_abi(&candidate, &current);
            for change in &diff.changes {
                println!("{change}");
            }
            println!(
                "current ABI version: {} (component {} {})",
                current.identity.abi_version, current.identity.name, current.identity.version
            );
            println!("summary: {}", diff.summary());
            if diff.has_breaking_changes() {
                return Err(format!("ABI check failed: {}", diff.summary()));
            }
            Ok(())
        }
        AbiAction::Export { input, output } => {
            let source = fs::read_to_string(&input)
                .map_err(|e| format!("read {}: {}", input.display(), e))?;
            let tokens = lexer::Lexer::new(&source)
                .tokenize()
                .map_err(|e| format!("lex {}: {}", input.display(), e))?;
            let file = mimi::parser::Parser::new(tokens)
                .parse_file()
                .map_err(|e| format!("parse {}: {}", input.display(), e))?;
            let checked = crate::emit::checked_component_input(&file)?;
            let extern_funcs = crate::emit::resolved_extern_funcs(&checked)?;
            let exported_funcs = crate::emit::resolved_exported_funcs(&checked, &extern_funcs)?;
            let type_defs = crate::emit::resolved_type_defs(&checked)?;

            let pkg = input
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("mimi_export")
                .to_string();
            let mut gen = AbiGenerator::new().identity(ComponentIdentity {
                name: pkg,
                version: mimi::component::MimiAbi::FORMAT_VERSION.to_string(),
                abi_version: 1,
            });
            register_core_runtime_abi(&mut gen);
            for (name, def) in &type_defs {
                let converted = match &def.kind {
                    ast::TypeDefKind::Alias(inner) | ast::TypeDefKind::Newtype(inner) => {
                        Some(AbiTypeDef::Alias(AbiAlias {
                            name: name.clone(),
                            target: mimi_type_to_abi(&type_string(inner)),
                        }))
                    }
                    ast::TypeDefKind::Record(fields) => {
                        let fields = fields
                            .iter()
                            .map(|f| AbiField {
                                name: f.name.clone(),
                                ty: mimi_type_to_abi(&type_string(&f.ty)),
                                offset: None,
                            })
                            .collect();
                        Some(AbiTypeDef::Struct(AbiStruct {
                            name: name.clone(),
                            fields,
                            is_repr_c: def.attributes.contains(&ast::TypeAttribute::ReprC),
                            size: None,
                            align: None,
                        }))
                    }
                    ast::TypeDefKind::Enum(variants)
                        if variants.iter().all(|v| v.payload.is_none()) =>
                    {
                        Some(AbiTypeDef::Enum(AbiEnum {
                            name: name.clone(),
                            variants: variants
                                .iter()
                                .enumerate()
                                .map(|(i, v)| (v.name.clone(), i as i64))
                                .collect(),
                            repr: AbiPrimitive::I32,
                        }))
                    }
                    _ => None,
                };
                if let Some(ty_def) = converted {
                    gen.type_def(ty_def);
                }
            }
            for ef in &extern_funcs {
                let name = ef.name.clone();
                let params = ef.params.clone();
                let ret = ef.ret.clone();
                gen.import(&name, |f| {
                    let mut b = f;
                    for p in &params {
                        b = b.param(&p.name, mimi_type_to_abi(&type_string(&p.ty)));
                    }
                    if let Some(ret) = &ret {
                        b = b.returns(mimi_type_to_abi(&type_string(ret)));
                    }
                    b
                });
            }
            for ef in &exported_funcs {
                let name = ef.name.clone();
                let params = ef.params.clone();
                let ret = ef.ret.clone();
                gen.export(&name, |f| {
                    let mut b = f;
                    for p in &params {
                        b = b.param(&p.name, mimi_type_to_abi(&type_string(&p.ty)));
                    }
                    if let Some(ret) = &ret {
                        b = b.returns(mimi_type_to_abi(&type_string(ret)));
                    }
                    b
                });
            }
            let abi = MimiAbi::from_component_ir(&gen.build());
            write_json(output.as_deref(), &abi)
        }
        AbiAction::EmitGo {
            input,
            output,
            module_name,
        } => {
            let abi = read_abi(&input)?;
            let module = module_name.unwrap_or_else(|| source_stem(&input));
            let funcs = abi_bridge::to_extern_funcs(&abi, false, "abi.emit_go");
            let types = abi_bridge::to_type_defs(&abi, "abi.emit_go");
            let code = ffi::go_bind::GoBindGenerator::new(types, &module)
                .generate(&funcs)
                .map_err(|e| format!("generate Go bindings: {e}"))?;
            write_text(output.as_deref(), &code)
        }
        AbiAction::EmitNode {
            input,
            output,
            ts_output,
            module_name,
        } => {
            let abi = read_abi(&input)?;
            let module = module_name.unwrap_or_else(|| source_stem(&input));
            let funcs = abi_bridge::to_extern_funcs(&abi, false, "abi.emit_node");
            let types = abi_bridge::to_type_defs(&abi, "abi.emit_node");
            let gen = ffi::node_bind::NodeBindGenerator::new(types, &module);
            let code = gen
                .generate(&funcs)
                .map_err(|e| format!("generate Node bindings: {e}"))?;
            write_text(output.as_deref(), &code)?;
            if let Some(ts_path) = ts_output {
                let dts = gen
                    .generate_dts(&funcs)
                    .map_err(|e| format!("generate TypeScript declarations: {e}"))?;
                std::fs::write(&ts_path, dts)
                    .map_err(|e| format!("write {}: {}", ts_path.display(), e))?;
            }
            Ok(())
        }
        AbiAction::EmitPy {
            input,
            output,
            module_name,
        } => {
            let abi = read_abi(&input)?;
            let module = module_name.unwrap_or_else(|| source_stem(&input));
            let funcs = abi_bridge::to_extern_funcs(&abi, true, "abi.emit_py");
            let types = abi_bridge::to_type_defs(&abi, "abi.emit_py");
            let code = ffi::py_bind::PyBindGenerator::new(types, &module)
                .generate(&funcs)
                .map_err(|e| format!("generate Python bindings: {e}"))?;
            write_text(output.as_deref(), &code)
        }
        AbiAction::EmitJava {
            input,
            output,
            java_output,
            module_name,
        } => {
            let abi = read_abi(&input)?;
            let module = module_name.unwrap_or_else(|| source_stem(&input));
            let funcs = abi_bridge::to_extern_funcs(&abi, false, "abi.emit_java");
            let types = abi_bridge::to_type_defs(&abi, "abi.emit_java");
            let gen = ffi::jni_bind::JniBindGenerator::new(types, &module);
            let c_bridge = gen
                .generate_c(&funcs)
                .map_err(|e| format!("generate JNI C bridge: {e}"))?;
            write_text(output.as_deref(), &c_bridge)?;
            if let Some(java_path) = java_output {
                let java = gen
                    .generate_java(&funcs)
                    .map_err(|e| format!("generate Java class: {e}"))?;
                std::fs::write(&java_path, java)
                    .map_err(|e| format!("write {}: {}", java_path.display(), e))?;
            }
            Ok(())
        }
        AbiAction::EmitCpp {
            input,
            output,
            module_name,
        } => {
            let abi = read_abi(&input)?;
            let module = module_name.unwrap_or_else(|| source_stem(&input));
            let funcs = abi_bridge::to_extern_funcs(&abi, false, "abi.emit_cpp");
            let types = abi_bridge::to_type_defs(&abi, "abi.emit_cpp");
            let code = ffi::cpp_bind::CppBindGenerator::new(types, &module)
                .generate(&funcs)
                .map_err(|e| format!("generate C++ bindings: {e}"))?;
            write_text(output.as_deref(), &code)
        }
    }
}

fn source_stem(input: &Path) -> String {
    input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mimi_component")
        .to_string()
}

fn write_text(output: Option<&Path>, text: &str) -> Result<(), String> {
    if let Some(path) = output {
        if path.as_os_str() == "-" {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(text.as_bytes())
                .map_err(|e| format!("write stdout: {e}"))
        } else {
            fs::write(path, text).map_err(|e| format!("write {}: {}", path.display(), e))
        }
    } else {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(text.as_bytes())
            .map_err(|e| format!("write stdout: {e}"))
    }
}

fn type_string(ty: &ast::Type) -> String {
    match ty {
        ast::Type::Located { ty, .. } => type_string(ty),
        ast::Type::Name(name, generics) => {
            if generics.is_empty() {
                name.clone()
            } else {
                let args: Vec<String> = generics.iter().map(type_string).collect();
                format!("{}<{}>", name, args.join(", "))
            }
        }
        ast::Type::Ref(lt, inner) => {
            let lt = lt.as_ref().map(|l| format!("'{} ", l)).unwrap_or_default();
            format!("&{}{}", lt, type_string(inner))
        }
        ast::Type::RefMut(lt, inner) => {
            let lt = lt.as_ref().map(|l| format!("'{} ", l)).unwrap_or_default();
            format!("&{}mut {}", lt, type_string(inner))
        }
        ast::Type::Option(inner) => format!("Option<{}>", type_string(inner)),
        ast::Type::Result(ok, err) => {
            format!("Result<{}, {}>", type_string(ok), type_string(err))
        }
        ast::Type::Tuple(types) => {
            let inner: Vec<String> = types.iter().map(type_string).collect();
            format!("({})", inner.join(", "))
        }
        ast::Type::Func(args, ret) => {
            let args: Vec<String> = args.iter().map(type_string).collect();
            format!("fn({}) -> {}", args.join(", "), type_string(ret))
        }
        ast::Type::ExternFunc(args, ret) => {
            let args: Vec<String> = args.iter().map(type_string).collect();
            format!(
                "extern \"C\" fn({}) -> {}",
                args.join(", "),
                type_string(ret)
            )
        }
        ast::Type::CBuffer(inner) => format!("c_buffer<{}>", type_string(inner)),
        ast::Type::Cap(name) => format!("cap {}", name),
        ast::Type::CapAtom(name) => format!("cap {}", name),
        ast::Type::Shared(inner) => format!("shared<{}>", type_string(inner)),
        ast::Type::Weak(inner) => format!("weak<{}>", type_string(inner)),
        ast::Type::Newtype(name, inner) => {
            format!("{} /* newtype({}) */", name, type_string(inner))
        }
        ast::Type::Array(inner, size) => format!("[{}; {}]", type_string(inner), size),
        ast::Type::Slice(inner) => format!("&[{}]", type_string(inner)),
        ast::Type::ImplTrait(traits) => format!("impl {}", traits.join(" + ")),
        ast::Type::DynTrait(traits) => format!("dyn {}", traits.join(" + ")),
        ast::Type::RawPtr(inner) => format!("*{}", type_string(inner)),
        ast::Type::RawPtrMut(inner) => format!("*mut {}", type_string(inner)),
        ast::Type::Nothing => "nothing".to_string(),
        ast::Type::Infer => "_".to_string(),
        ast::Type::TypeVar(id) => format!("?T{}", id),
        ast::Type::ForAll(params, body) => {
            format!("forall {}. {}", params.join(", "), type_string(body))
        }
        ast::Type::TyErr => "«error»".to_string(),
    }
}

fn read_abi(path: &Path) -> Result<MimiAbi, String> {
    let json = read_text(Some(path))?;
    MimiAbi::from_json_validated(&json)
        .map_err(|e: MimiAbiError| format!("invalid .mimiabi {}: {}", path.display(), e))
}

fn read_text(path: Option<&Path>) -> Result<String, String> {
    match path {
        Some(p) if p.as_os_str() == "-" => {
            let mut buf = String::new();
            std::io::stdin()
                .lock()
                .read_to_string(&mut buf)
                .map_err(|e| format!("read stdin: {e}"))?;
            Ok(buf)
        }
        Some(p) => fs::read_to_string(p).map_err(|e| format!("read {}: {}", p.display(), e)),
        None => Err("missing path".to_string()),
    }
}

fn write_json(output: Option<&Path>, abi: &MimiAbi) -> Result<(), String> {
    let json = abi
        .to_json()
        .map_err(|e| format!("serialize .mimiabi: {e}"))?;
    let text = format!("{json}\n");
    write_text(output, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use clap::Parser;
    use std::path::PathBuf;

    fn test_dir(tag: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mimi_abi_test_{}_{}_{}",
            tag,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    #[test]
    fn abi_core_export_validate_roundtrip() {
        let dir = test_dir("core");
        let abi_path = dir.join("core.mimiabi");

        run(AbiAction::Core {
            output: Some(abi_path.clone()),
        })
        .expect("core export should succeed");

        run(AbiAction::Validate {
            input: abi_path.clone(),
        })
        .expect("core abi should validate");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_validate_rejects_bad_json() {
        let dir = test_dir("bad");
        let bad_path = dir.join("bad.mimiabi");
        std::fs::write(&bad_path, "{not-json").expect("write bad abi");

        let err = run(AbiAction::Validate { input: bad_path }).expect_err("bad abi must fail");
        assert!(err.contains("invalid .mimiabi"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_diff_identical_core_has_no_changes() {
        let dir = test_dir("diff");
        let left = dir.join("left.mimiabi");
        let right = dir.join("right.mimiabi");

        run(AbiAction::Core {
            output: Some(left.clone()),
        })
        .expect("export left");
        // Copy the same file so both inputs are byte-identical.
        std::fs::copy(&left, &right).expect("copy abi");

        run(AbiAction::Diff { left, right }).expect("identical abi diff should succeed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_emit_c_and_rust_from_core() {
        let dir = test_dir("emit");
        let abi_path = dir.join("core.mimiabi");
        let c_path = dir.join("core.h");
        let rust_path = dir.join("core.rs");

        run(AbiAction::Core {
            output: Some(abi_path.clone()),
        })
        .expect("export core");

        run(AbiAction::EmitC {
            input: abi_path.clone(),
            output: Some(c_path.clone()),
        })
        .expect("emit C header");

        run(AbiAction::EmitRust {
            input: abi_path,
            output: Some(rust_path.clone()),
        })
        .expect("emit Rust bindings");

        let c = std::fs::read_to_string(&c_path).expect("read C header");
        let rust = std::fs::read_to_string(&rust_path).expect("read Rust bindings");
        assert!(
            c.contains("#include"),
            "C header should be generated, got: {c}"
        );
        assert!(
            rust.contains("extern \"C\""),
            "Rust bindings should be generated, got: {rust}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_check_core_matches_itself() {
        let dir = test_dir("check_ok");
        let abi_path = dir.join("core.mimiabi");
        run(AbiAction::Core {
            output: Some(abi_path.clone()),
        })
        .expect("export core");

        run(AbiAction::Check { input: abi_path }).expect("current core ABI should match itself");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_check_rejects_renamed_export() {
        let dir = test_dir("check_bad");
        let abi_path = dir.join("core.mimiabi");
        let bad_path = dir.join("bad.mimiabi");
        run(AbiAction::Core {
            output: Some(abi_path.clone()),
        })
        .expect("export core");

        let json = std::fs::read_to_string(&abi_path).expect("read core abi");
        let altered = json.replacen("\"mimi_rc_alloc\"", "\"mimi_rc_alloc_renamed\"", 1);
        std::fs::write(&bad_path, altered).expect("write altered abi");

        let err = run(AbiAction::Check { input: bad_path })
            .expect_err("renamed export must be a breaking ABI change");
        assert!(err.contains("ABI check failed"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_export_source_contains_imports_and_exports() {
        let dir = test_dir("export");
        let source_path = dir.join("sample.mimi");
        let abi_path = dir.join("sample.mimiabi");
        std::fs::write(
            &source_path,
            r#"
extern "C" {
    func imported(x: i32) -> i32
}
extern "C" func exported(x: i32) -> i32 { x }
#[repr(C)]
type Point { x: i32, y: i32 }
"#,
        )
        .expect("write source");

        run(AbiAction::Export {
            input: source_path,
            output: Some(abi_path.clone()),
        })
        .expect("export source ABI");

        let json = std::fs::read_to_string(&abi_path).expect("read exported ABI");
        let abi = MimiAbi::from_json_validated(&json).expect("exported ABI must validate");
        assert!(
            abi.exports.iter().any(|s| s.name == "exported"),
            "missing export"
        );
        assert!(
            abi.imports.iter().any(|s| s.name == "imported"),
            "missing import"
        );
        assert!(
            abi.exports.iter().any(|s| s.name == "mimi_rc_alloc"),
            "core runtime ABI missing"
        );
        assert!(
            abi.types.iter().any(|t| format!("{t:?}").contains("Point")),
            "user type definition missing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_export_feeds_emit_c_and_rust() {
        let dir = test_dir("export_emit");
        let source_path = dir.join("sample.mimi");
        let abi_path = dir.join("sample.mimiabi");
        let c_path = dir.join("sample.h");
        let rust_path = dir.join("sample.rs");
        std::fs::write(
            &source_path,
            r#"
extern "C" func exported(x: i32) -> i32 { x }
#[repr(C)]
type Point { x: i32, y: i32 }
"#,
        )
        .expect("write source");

        run(AbiAction::Export {
            input: source_path,
            output: Some(abi_path.clone()),
        })
        .expect("export source ABI");
        run(AbiAction::EmitC {
            input: abi_path.clone(),
            output: Some(c_path.clone()),
        })
        .expect("emit C header");
        run(AbiAction::EmitRust {
            input: abi_path,
            output: Some(rust_path.clone()),
        })
        .expect("emit Rust bindings");

        let c = std::fs::read_to_string(&c_path).expect("read C header");
        let rust = std::fs::read_to_string(&rust_path).expect("read Rust bindings");
        assert!(c.contains("typedef struct Point"), "missing Point in C");
        assert!(rust.contains("pub struct Point"), "missing Point in Rust");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_emit_all_legacy_languages_from_core() {
        let dir = test_dir("emit_all");
        let abi_path = dir.join("core.mimiabi");
        run(AbiAction::Core {
            output: Some(abi_path.clone()),
        })
        .expect("export core ABI");

        let mut outputs = Vec::new();
        for (lang, suffix) in [
            ("go", "go"),
            ("node", "node.c"),
            ("py", "cpp"),
            ("java", "c"),
            ("cpp", "hpp"),
        ] {
            let out = dir.join(format!("core_{}.{}", lang, suffix));
            match lang {
                "go" => run(AbiAction::EmitGo {
                    input: abi_path.clone(),
                    output: Some(out.clone()),
                    module_name: None,
                }),
                "node" => run(AbiAction::EmitNode {
                    input: abi_path.clone(),
                    output: Some(out.clone()),
                    ts_output: None,
                    module_name: None,
                }),
                "py" => run(AbiAction::EmitPy {
                    input: abi_path.clone(),
                    output: Some(out.clone()),
                    module_name: None,
                }),
                "java" => run(AbiAction::EmitJava {
                    input: abi_path.clone(),
                    output: Some(out.clone()),
                    java_output: None,
                    module_name: None,
                }),
                "cpp" => run(AbiAction::EmitCpp {
                    input: abi_path.clone(),
                    output: Some(out.clone()),
                    module_name: None,
                }),
                _ => unreachable!(),
            }
            .unwrap_or_else(|e| panic!("{lang} from .mimiabi failed: {e}"));
            outputs.push((lang, out));
        }

        let text = |path: &Path| std::fs::read_to_string(path).expect("read output");
        let go = text(&outputs[0].1);
        assert!(go.contains("package core"), "Go output missing package");
        let node = text(&outputs[1].1);
        assert!(
            node.contains("node_api.h"),
            "Node output missing N-API include"
        );
        let py = text(&outputs[2].1);
        assert!(
            py.contains("pybind11/pybind11.h"),
            "Python output missing pybind11"
        );
        let java = text(&outputs[3].1);
        assert!(java.contains("jni.h"), "Java output missing JNI include");
        let cpp = text(&outputs[4].1);
        assert!(
            cpp.contains("#include <cstdint>"),
            "C++ output missing include"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abi_cli_subcommand_clap_parses() {
        let args = vec!["mimi", "abi", "hash", "/tmp/core.mimiabi"];
        let parsed = crate::Args::parse_from(args);
        assert!(matches!(parsed.cmd, Command::Abi { .. }));
    }
}
