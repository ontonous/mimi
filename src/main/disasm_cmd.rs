//! `mimi disasm` — disassemble a .mimi file to bytecode.

use std::path::Path;
use std::process;

use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::interp::bytecode::mir::MirBytecodeError;

fn render_mir_bytecode_error(error: &MirBytecodeError) -> String {
    if error.diagnostic_code().is_some() {
        crate::format_cli_error(&error.to_string())
    } else {
        format!("  {error}")
    }
}

pub fn disasm_file(path: &Path) -> ! {
    // Full audit 2026-08-05 §13: route through the 100 MiB capped read used
    // by every other command; a raw fs::read_to_string on a multi-GB file
    // would exhaust memory before parsing even starts.
    let source = match mimi::path_safety::read_source_capped(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    };

    let tokens = match mimi::lexer::Lexer::new(&source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: lexer: {}", e);
            process::exit(1);
        }
    };

    // Preserve the same disk source identities used by run/build/verify and
    // MIR inspection so canonical disassembly observes the same graph receipt.
    let parser = match mimi::loader::parser_for_path(tokens, path) {
        Ok(parser) => parser,
        Err(error) => {
            eprintln!("error: parse source identity: {error}");
            process::exit(1);
        }
    };
    let file = match parser.parse_file() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: parse: {}", e);
            process::exit(1);
        }
    };

    // Keep disassembly on the same checked-program boundary as the other
    // production CLI entries.  The canonical route below consumes only the
    // resulting MIR; it never asks the bytecode consumer to rediscover types
    // or ownership from this source file.
    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = mimi::loader::ModuleLoader::new(base_dir);
        if let Err(error) = loader.load_main_with_file(path, file) {
            eprintln!("error: import loading failed: {}", error);
            process::exit(1);
        }
        match loader.merge_all() {
            Ok(merged) => merged,
            Err(error) => {
                eprintln!("error: import merge failed: {}", error);
                process::exit(1);
            }
        }
    } else {
        file
    };
    mimi::loader::merge_prelude_into(&mut merged_file);

    let checked_program = match mimi::core::check_program(&merged_file) {
        Ok(program) => program,
        Err(diagnostics) => {
            let use_color = colors_enabled();
            let filename = path.display().to_string();
            for diagnostic in &diagnostics {
                let formatted = format_diagnostic(diagnostic, Some(source.as_str()), &filename);
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            }
            eprintln!("error: type checking failed");
            process::exit(1);
        }
    };

    // S14: disasm is a consumer of the already-closed scalar-collection
    // island.  A recognized island that fails canonical preflight is a hard
    // error; it must not regain the old compiler as an implicit fallback.
    let program = match crate::canonical_dispatch::select_default_route(
        &checked_program,
        &merged_file,
    ) {
        crate::canonical_dispatch::DefaultMirRoute::Canonical(canonical) => {
            let profile = crate::canonical_dispatch::route_receipt_profile_for_checked(
                &checked_program,
                "disasm-canonical-v1",
            );
            let receipt = canonical.route_receipt(profile);
            if std::env::var_os("MIMI_ROUTE_CENSUS").is_some() {
                eprintln!(
                    "MIMI_ROUTE_CENSUS route-admission consumer=disasm mode=default checker_profile={} mir_digest={} receipt_identity_digest={}",
                    profile,
                    canonical.canonical_digest(),
                    receipt.route_identity_digest(),
                );
            }
            let has_owned_get_result_branch_v3 = checked_program
                .resource_analyses()
                .values()
                .any(|analysis| analysis.has_ordinary_map_owned_get_result_branch_v3_receipt());
            let bytecode = if profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND
                && has_owned_get_result_branch_v3
            {
                mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_get_result_branch_v3_route(
                    &checked_program,
                    &canonical,
                )
            } else if profile
                == mimi::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L1_CORE_PROFILE
            {
                // The direct tuple owner slice uses the same Checker-bound
                // L1 adapter as run/test; L2 remains verifier-only.
                mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l1_route(
                        &checked_program,
                        &canonical,
                        &receipt,
                    )
            } else if profile == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE
            {
                // DynamicArg0 is an AST-free opcode authorized only by
                // the exact CheckedProgram/MIR receipt. Disassembly uses
                // the same route binder as execution consumers.
                mimi::interp::bytecode::compile_mir_program_with_dynamic_keys_get_route(
                    &checked_program,
                    &canonical,
                )
            } else if profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE
            {
                mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
                    &checked_program,
                    &canonical,
                )
            } else {
                mimi::interp::bytecode::compile_mir_program_with_route_receipt(&canonical, &receipt)
            };
            match bytecode {
                Ok(program) => program,
                Err(errors) => {
                    eprintln!("error: canonical MIR bytecode is not eligible:");
                    for error in errors {
                        // Route receipt/manifest failures must keep their
                        // registered code even though disasm exits
                        // directly instead of returning through the
                        // top-level CLI error boundary.
                        eprintln!("{}", render_mir_bytecode_error(&error));
                    }
                    process::exit(1);
                }
            }
        }
        crate::canonical_dispatch::DefaultMirRoute::Rejected(reason) => {
            let message = format!("default Canonical MIR route rejected: {reason}");
            if mimi::diagnostic::codes::canonical_mir_route_code_in_message(&message).is_some() {
                eprintln!("{}", crate::format_cli_error(&message));
            } else {
                eprintln!("error: {}", message);
            }
            process::exit(1);
        }
        crate::canonical_dispatch::DefaultMirRoute::Legacy(_reason) => {
            crate::canonical_dispatch::report_legacy_route(_reason);
            let mut compiler = mimi::interp::bytecode::BytecodeCompiler::new();
            compiler.install_checked_program(&checked_program);
            match compiler.compile_file(&merged_file) {
                Ok(program) => program,
                Err(e) => {
                    eprintln!("error: compilation failed: {}", e);
                    process::exit(1);
                }
            }
        }
        crate::canonical_dispatch::DefaultMirRoute::VerifierOnlyFfiStringRecordOwnerSlice => {
            eprintln!("error: verifier-only FFI owner slice has no bytecode representation");
            process::exit(1);
        }
        crate::canonical_dispatch::DefaultMirRoute::VerifierOnlyDirectTupleGetUnpackL2 => {
            eprintln!(
                "error: direct tuple Get L2 semantic projection has no bytecode representation"
            );
            process::exit(1);
        }
    };

    print!(
        "{}",
        mimi::interp::bytecode::disasm::disassemble_program(&program)
    );
    process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::render_mir_bytecode_error;
    use mimi::diagnostic::format::strip_ansi;
    use mimi::interp::bytecode::mir::MirBytecodeError;

    #[test]
    fn disasm_route_error_renderer_keeps_registered_code() {
        let error = MirBytecodeError {
            function: mimi::core::resolved::NodeId("mir-program".into()),
            message: "MIR-RECEIPT-MANIFEST-001: future field".into(),
        };
        let rendered = strip_ansi(&render_mir_bytecode_error(&error));
        assert!(
            rendered.starts_with("error[MIR-RECEIPT-MANIFEST-001] "),
            "{rendered}"
        );
        assert_eq!(
            rendered.matches("MIR-RECEIPT-MANIFEST-001").count(),
            1,
            "{rendered}"
        );
    }

    #[test]
    fn disasm_route_error_renderer_matches_direct_diagnostic_for_wrapped_code() {
        let error = MirBytecodeError {
            function: mimi::core::resolved::NodeId("mir-program".into()),
            message: format!(
                "bytecode wrapper: {}: canonical route manifest rejected: future field",
                mimi::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
            ),
        };
        let rendered = strip_ansi(&render_mir_bytecode_error(&error));
        let direct = strip_ansi(&mimi::diagnostic::format::format_diagnostic(
            &error.to_diagnostic(),
            None,
            "",
        ));

        assert_eq!(rendered, direct);
        assert_eq!(
            rendered,
            "error[MIR-RECEIPT-MANIFEST-001] MIR bytecode 'mir-program': bytecode wrapper: canonical route manifest rejected: future field\n"
        );
        assert_eq!(rendered.matches("MIR-RECEIPT-MANIFEST-001").count(), 1);
    }

    #[test]
    fn disasm_plain_error_renderer_keeps_indented_legacy_shape() {
        let error = MirBytecodeError {
            function: mimi::core::resolved::NodeId("bad".into()),
            message: "unsupported MIR instruction".into(),
        };
        assert_eq!(
            render_mir_bytecode_error(&error),
            "  MIR bytecode 'bad': unsupported MIR instruction"
        );
    }
}
