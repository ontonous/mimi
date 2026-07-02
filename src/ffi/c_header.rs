//! C header generation for extern blocks and extern-exported Mimi functions.
//!
//! This module generates C header files from Mimi extern declarations,
//! allowing C code to call Mimi functions safely.

use std::collections::HashMap;
use std::fmt::Write;

use crate::ast::{
    ExternFunc, ExternParam, FuncDef, Type, TypeAttribute, TypeDef, TypeDefKind, VariantPayload,
};
use crate::ffi::contract::{FfiArgContract, FfiContract};

/// C header generator
pub struct CHeaderGenerator {
    /// Type definitions for generating struct/enum declarations
    type_defs: HashMap<String, TypeDef>,
}

impl CHeaderGenerator {
    /// Create a new C header generator
    pub fn new(type_defs: HashMap<String, TypeDef>) -> Self {
        Self { type_defs }
    }

    /// Generate a complete C header file from extern blocks
    pub fn generate(&self, extern_funcs: &[ExternFunc]) -> Result<String, std::fmt::Error> {
        let mut header = String::new();

        // Header guard
        writeln!(header, "#ifndef MIMI_FFI_H")?;
        writeln!(header, "#define MIMI_FFI_H")?;
        writeln!(header)?;
        writeln!(header, "#include <stdint.h>")?;
        writeln!(header, "#include <stdbool.h>")?;
        writeln!(header)?;

        // Opaque handle type (all cross-boundary references use int64_t)
        writeln!(header, "typedef int64_t MimiHandle;")?;
        writeln!(header)?;

        // Shared handle API
        writeln!(header, "/** Retain a shared handle (increment C-side reference count). Returns the handle ID. */")?;
        writeln!(header, "MimiHandle mimi_shared_retain(MimiHandle handle);")?;
        writeln!(header, "/** Release a shared handle (decrement reference count). Removed from table at zero. */")?;
        writeln!(header, "void mimi_shared_release(MimiHandle handle);")?;
        writeln!(header, "/** Get a raw pointer to the inner value. Pointer valid only while handle alive. Returns NULL if invalid. */")?;
        writeln!(header, "void* mimi_shared_get_ptr(MimiHandle handle);")?;
        writeln!(
            header,
            "/** Create a shared handle from a heap-allocated Value (transfers ownership). */"
        )?;
        writeln!(header, "MimiHandle mimi_shared_create(void* value);")?;
        writeln!(header)?;

        // Value API
        writeln!(header, "// Value constructors / accessors")?;
        writeln!(
            header,
            "/** Free a Value obtained via mimi_shared_get_ptr or mimi_value_new_*. */"
        )?;
        writeln!(header, "void mimi_value_free(void* value);")?;
        writeln!(
            header,
            "/** Create a new integer Value (caller frees with mimi_value_free). */"
        )?;
        writeln!(header, "void* mimi_value_new_int(int64_t n);")?;
        writeln!(
            header,
            "/** Create a new boolean Value (caller frees with mimi_value_free). */"
        )?;
        writeln!(header, "void* mimi_value_new_bool(bool b);")?;
        writeln!(
            header,
            "/** Create a new floating-point Value (caller frees with mimi_value_free). */"
        )?;
        writeln!(header, "void* mimi_value_new_float(double f);")?;
        writeln!(
            header,
            "/** Read an integer from a Value, or 0 if not an integer. */"
        )?;
        writeln!(header, "int64_t mimi_value_as_int(void* value);")?;
        writeln!(
            header,
            "/** Read a boolean from a Value, or false if not a boolean. */"
        )?;
        writeln!(header, "bool mimi_value_as_bool(void* value);")?;
        writeln!(
            header,
            "/** Read a float from a Value, or 0.0 if not a float. */"
        )?;
        writeln!(header, "double mimi_value_as_float(void* value);")?;
        writeln!(header)?;

        // Cap API
        writeln!(
            header,
            "/** Opaque capability handle — see mimi_cap_check / mimi_cap_consume. */"
        )?;
        writeln!(header, "typedef int64_t MimiCap;")?;
        writeln!(
            header,
            "/** Check if a capability is valid and matches name (non-consuming). */"
        )?;
        writeln!(
            header,
            "bool mimi_cap_check(MimiCap cap, const char* name);"
        )?;
        writeln!(
            header,
            "/** Consume a capability (mark as used). Returns true if valid and consumed. */"
        )?;
        writeln!(
            header,
            "bool mimi_cap_consume(MimiCap cap, const char* name);"
        )?;
        writeln!(
            header,
            "/** Register a new capability and return its opaque handle. */"
        )?;
        writeln!(header, "MimiCap mimi_cap_register(const char* name);")?;
        writeln!(header)?;

        // String API
        writeln!(header, "/** Get a C string pointer from a Mimi string (borrow). Caller must NOT free the result. */")?;
        writeln!(
            header,
            "const char* mimi_string_as_c_str(void* mimi_string);"
        )?;
        writeln!(header, "/** Transfer string ownership to C. Caller must call mimi_string_free_raw() on result. */")?;
        writeln!(header, "char* mimi_string_into_raw(void* mimi_string);")?;
        writeln!(
            header,
            "/** Create a Mimi string from a C string (takes ownership of c_str). */"
        )?;
        writeln!(header, "void* mimi_string_from_raw(char* c_str);")?;
        writeln!(header, "/** Free a string obtained via mimi_string_into_raw() or C-allocated strings returned by extern functions. */")?;
        writeln!(header, "void mimi_string_free_raw(char* c_str);")?;
        writeln!(header, "/** Free a C string pointer obtained from mimi_string_as_c_str() when no longer needed. */")?;
        writeln!(header, "void mimi_string_as_c_str_free(const char* c_str);")?;
        writeln!(
            header,
            "/** Free all pending C strings allocated by mimi_string_as_c_str() on this thread. */"
        )?;
        writeln!(header, "void mimi_string_as_c_str_free_all(void);")?;
        writeln!(
            header,
            "/** Return the byte length of a Mimi string value, or -1 on error. */"
        )?;
        writeln!(header, "int64_t mimi_string_len(void* mimi_string);")?;
        writeln!(header, "/** Free a C string allocated by the Mimi runtime (e.g. returned by extern functions). */")?;
        writeln!(header, "void mimi_string_free(char* c_str);")?;
        writeln!(header)?;

        // Callback / runtime misc API
        writeln!(header, "// Runtime misc API")?;
        writeln!(
            header,
            "/** Callback signature for the runtime error handler. */"
        )?;
        writeln!(
            header,
            "typedef void (*MimiErrorHandler)(const char* message);"
        )?;
        writeln!(header, "/** Set a global error handler invoked on FFI contract violations. Pass NULL to reset. */")?;
        writeln!(
            header,
            "void mimi_runtime_set_error_handler(MimiErrorHandler handler);"
        )?;
        writeln!(
            header,
            "/** Deregister a callback previously passed to C. Safe to call from any thread. */"
        )?;
        writeln!(
            header,
            "void mimi_callback_deregister(int64_t callback_id);"
        )?;
        writeln!(
            header,
            "/** Submit a raw task to the global Mimi thread pool. */"
        )?;
        writeln!(
            header,
            "void mimi_pool_submit(void* (*fn_ptr)(void*), void* arg);"
        )?;
        writeln!(
            header,
            "/** Block until all submitted pool tasks complete. */"
        )?;
        writeln!(header, "void mimi_pool_join_all(void);")?;
        writeln!(header)?;

        // Generate struct definitions for #[repr(C)] types
        writeln!(header, "// Type definitions")?;
        self.generate_type_definitions(&mut header)?;
        writeln!(header)?;

        // Function declarations
        writeln!(header, "// Function declarations")?;
        for func in extern_funcs {
            self.generate_function_declaration(&mut header, func)?;
        }

        // Note: the closing #endif is emitted by the caller, after any
        // exported Mimi function declarations have been appended. This
        // matches the user's repro from examples/ffi/math.mimi where
        // extern "C" funcs end up below the guard otherwise and the C
        // preprocessor drops them.

        Ok(header)
    }

    /// Generate C type definitions for #[repr(C)] types
    fn generate_type_definitions(&self, header: &mut String) -> Result<(), std::fmt::Error> {
        for (name, type_def) in &self.type_defs {
            // Only generate for types with #[repr(C)] attribute
            if !type_def.attributes.contains(&TypeAttribute::ReprC) {
                continue;
            }

            match &type_def.kind {
                TypeDefKind::Record(fields) => {
                    writeln!(header, "typedef struct {} {{", name)?;
                    for field in fields {
                        let c_type = self.mimi_type_to_c_type(&field.ty);
                        writeln!(header, "    {} {};", c_type, field.name)?;
                    }
                    writeln!(header, "}} {};", name)?;
                    writeln!(header)?;
                }
                TypeDefKind::Enum(variants) => {
                    // Check if any variant has a payload
                    let has_payload = variants.iter().any(|v| v.payload.is_some());
                    if has_payload && type_def.attributes.contains(&TypeAttribute::ReprC) {
                        // Generate as struct with tag + union for #[repr(C)] enums with payloads
                        writeln!(header, "typedef struct {} {{", name)?;
                        writeln!(header, "    int32_t tag;")?;
                        writeln!(header, "    union {{")?;
                        for variant in variants {
                            let field_name = format!("payload_{}", variant.name);
                            if let Some(payload) = &variant.payload {
                                match payload {
                                    VariantPayload::Tuple(types) => {
                                        if types.len() == 1 {
                                            let c_type = self.mimi_type_to_c_type(&types[0]);
                                            writeln!(header, "        {} {};", c_type, field_name)?;
                                        } else {
                                            // Multi-field tuple: generate as struct in union
                                            writeln!(header, "        struct {{")?;
                                            for (j, t) in types.iter().enumerate() {
                                                let c_type = self.mimi_type_to_c_type(t);
                                                writeln!(
                                                    header,
                                                    "            {} field_{};",
                                                    c_type, j
                                                )?;
                                            }
                                            writeln!(header, "        }} {};", field_name)?;
                                        }
                                    }
                                    VariantPayload::Record(fields) => {
                                        writeln!(header, "        struct {{")?;
                                        for f in fields {
                                            let c_type = self.mimi_type_to_c_type(&f.ty);
                                            writeln!(header, "            {} {};", c_type, f.name)?;
                                        }
                                        writeln!(header, "        }} {};", field_name)?;
                                    }
                                }
                            } else {
                                // No-payload variants need a dummy to keep union non-empty
                                writeln!(header, "        int8_t {};", field_name)?;
                            }
                        }
                        writeln!(header, "    }} data;")?;
                        writeln!(header, "}} {};", name)?;
                    } else {
                        // Generate simple enum with integer tag
                        writeln!(header, "typedef enum {} {{", name)?;
                        for (i, variant) in variants.iter().enumerate() {
                            writeln!(header, "    {}_{} = {},", name, variant.name, i)?;
                        }
                        writeln!(header, "}} {};", name)?;
                    }
                    writeln!(header)?;
                }
                TypeDefKind::Union(fields) => {
                    // Generate C union
                    writeln!(header, "typedef union {} {{", name)?;
                    for field in fields {
                        let c_type = self.mimi_type_to_c_type(&field.ty);
                        writeln!(header, "    {} {};", c_type, field.name)?;
                    }
                    writeln!(header, "}} {};", name)?;
                    writeln!(header)?;
                }
                TypeDefKind::Alias(ty) => {
                    let c_type = self.mimi_type_to_c_type(ty);
                    writeln!(header, "typedef {} {};", c_type, name)?;
                    writeln!(header)?;
                }
                TypeDefKind::Newtype(ty) => {
                    let c_type = self.mimi_type_to_c_type(ty);
                    writeln!(header, "typedef struct {} {{", name)?;
                    writeln!(header, "    {} value;", c_type)?;
                    writeln!(header, "}} {};", name)?;
                    writeln!(header)?;
                }
            }
        }
        Ok(())
    }

    /// Generate a C function declaration from a Mimi extern function
    fn generate_function_declaration(
        &self,
        header: &mut String,
        func: &ExternFunc,
    ) -> Result<(), std::fmt::Error> {
        let record_type_names: std::collections::HashSet<String> = self
            .type_defs
            .iter()
            .filter(|(_, td)| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
            .map(|(name, _)| name.clone())
            .collect();
        let repr_c_record_names: std::collections::HashSet<String> = self
            .type_defs
            .iter()
            .filter(|(_, td)| td.attributes.contains(&crate::ast::TypeAttribute::ReprC))
            .map(|(name, _)| name.clone())
            .collect();
        let contract = FfiContract::from_extern_with_caps_repr(
            func,
            &std::collections::HashSet::new(),
            &record_type_names,
            &repr_c_record_names,
        );

        // Return type
        let ret_type = self.contract_ret_to_c_type(&contract);

        // Function name
        write!(header, "{} {}(", ret_type, func.name)?;

        // Parameters
        for (i, param) in func.params.iter().enumerate() {
            if i > 0 {
                write!(header, ", ")?;
            }
            let c_type = self.contract_arg_to_c_type(&contract, i, &param.name);
            write!(header, "{}", c_type)?;
        }

        writeln!(header, ");")?;

        // Add comment with contract info if present
        if let Some(requires) = &func.requires {
            writeln!(header, "    // Requires: {:?}", requires)?;
        }
        if let Some(ensures) = &func.ensures {
            writeln!(header, "    // Ensures: {:?}", ensures)?;
        }
        Ok(())
    }

    /// Convert a Mimi type to a C type string
    fn mimi_type_to_c_type(&self, ty: &Type) -> String {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" => "int32_t".to_string(),
                "i64" => "int64_t".to_string(),
                "f64" => "double".to_string(),
                "bool" => "bool".to_string(),
                "string" => "const char*".to_string(),
                "unit" | "nothing" => "void".to_string(),
                _ => {
                    // Check if it's a known type definition
                    if self.type_defs.contains_key(name) {
                        name.clone()
                    } else {
                        "void*".to_string() // Unknown type, use void pointer
                    }
                }
            },
            Type::RawPtr(inner) => {
                let inner_type = self.mimi_type_to_c_type(inner);
                format!("{}*", inner_type)
            }
            Type::RawPtrMut(inner) => {
                let inner_type = self.mimi_type_to_c_type(inner);
                format!("{}*", inner_type)
            }
            Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_) => "MimiHandle".to_string(),
            Type::Cap(_) => "MimiCap".to_string(),
            Type::RawString => "char*".to_string(),
            Type::Infer => "void".to_string(),
            Type::Shared(inner) | Type::LocalShared(inner) => {
                let inner_type = self.mimi_type_to_c_type(inner);
                format!("MimiHandle /* shared {} */", inner_type)
            }
            Type::Ref(_, inner) | Type::RefMut(_, inner) => {
                let inner_type = self.mimi_type_to_c_type(inner);
                format!("{}*", inner_type)
            }
            _ => "void*".to_string(), // Complex types use void pointer
        }
    }

    /// Convert an FFI argument contract to a C type string
    fn contract_arg_to_c_type(&self, contract: &FfiContract, index: usize, name: &str) -> String {
        if index >= contract.args.len() {
            return format!("void* {}", name);
        }

        match &contract.args[index] {
            FfiArgContract::Int(scalar) => match scalar {
                crate::ffi::contract::FfiScalarType::I32 => format!("int32_t {}", name),
                crate::ffi::contract::FfiScalarType::I64 => format!("int64_t {}", name),
                crate::ffi::contract::FfiScalarType::Bool => format!("bool {}", name),
            },
            FfiArgContract::Float => format!("double {}", name),
            FfiArgContract::StringBorrow => format!("const char* {}", name),
            FfiArgContract::StringTransfer => format!("char* {}", name),
            FfiArgContract::Cap(_) => format!("MimiCap {}", name),
            FfiArgContract::RawPtr(inner) | FfiArgContract::RawPtrMut(inner) => {
                format!("{}* {}", self.mimi_type_to_c_type(inner), name)
            }
            FfiArgContract::CShared(inner)
            | FfiArgContract::CBorrow(inner)
            | FfiArgContract::CBorrowMut(inner) => {
                format!(
                    "MimiHandle /* {} */ {}",
                    self.mimi_type_to_c_type(inner),
                    name
                )
            }
            FfiArgContract::Json => format!("const char* {}", name),
            FfiArgContract::StructByValue(type_name) => format!("struct {} {}", type_name, name),
            FfiArgContract::Callback {
                param_types,
                ret_type,
            } => {
                let ret_c = self.mimi_type_to_c_type(ret_type);
                let params_c: Vec<String> = param_types
                    .iter()
                    .map(|t| self.mimi_type_to_c_type(t))
                    .collect();
                let params_str = if params_c.is_empty() {
                    "void".to_string()
                } else {
                    params_c.join(", ")
                };
                format!("{} (*{})({})", ret_c, name, params_str)
            }
            FfiArgContract::Unsupported(_) => format!("void* {}", name),
        }
    }

    /// Convert an FFI return contract to a C type string
    fn contract_ret_to_c_type(&self, contract: &FfiContract) -> String {
        match &contract.ret {
            crate::ffi::contract::FfiRetContract::Unit => "void".to_string(),
            crate::ffi::contract::FfiRetContract::Int(scalar) => match scalar {
                crate::ffi::contract::FfiScalarType::I32 => "int32_t".to_string(),
                crate::ffi::contract::FfiScalarType::I64 => "int64_t".to_string(),
                crate::ffi::contract::FfiScalarType::Bool => "bool".to_string(),
            },
            crate::ffi::contract::FfiRetContract::Float => "double".to_string(),
            crate::ffi::contract::FfiRetContract::String => "/*borrowed*/ char*".to_string(),
            crate::ffi::contract::FfiRetContract::StringOwned => "/*owned*/ char*".to_string(),
            crate::ffi::contract::FfiRetContract::Json => "char*".to_string(),
            crate::ffi::contract::FfiRetContract::StructByValue(type_name) => {
                format!("struct {}", type_name)
            }
            crate::ffi::contract::FfiRetContract::RawPtr(inner)
            | crate::ffi::contract::FfiRetContract::RawPtrMut(inner) => {
                format!("{}*", self.mimi_type_to_c_type(inner))
            }
            crate::ffi::contract::FfiRetContract::CShared(inner)
            | crate::ffi::contract::FfiRetContract::CBorrow(inner)
            | crate::ffi::contract::FfiRetContract::CBorrowMut(inner) => {
                format!("MimiHandle /* {} */", self.mimi_type_to_c_type(inner))
            }
            crate::ffi::contract::FfiRetContract::Unsupported(_) => "void*".to_string(),
        }
    }
}

/// Generate a C header that also includes Mimi→C exported functions.
/// `exported_funcs` are Mimi functions marked `extern "C"`.
pub fn generate_c_header_with_exported(
    extern_funcs: &[ExternFunc],
    exported_funcs: &[FuncDef],
    type_defs: HashMap<String, TypeDef>,
) -> Result<String, String> {
    let generator = CHeaderGenerator::new(type_defs);
    let mut header = generator.generate(extern_funcs).unwrap_or_default();

    if !exported_funcs.is_empty() {
        let _ = writeln!(&mut header, "\n// Exported Mimi functions (extern \"C\")");
        for func in exported_funcs {
            generator
                .generate_exported_func_decl(&mut header, func)
                .map_err(|e| format!("Failed to generate exported func decl: {}", e))?;
        }
    }

    // P0-6: close the header guard AFTER the exported function
    // declarations so the C preprocessor actually includes them. The
    // earlier code emitted `#endif` inside `generate()`, which placed it
    // before the appended export block and silently dropped every
    // `extern "C"` function from the resulting header.
    let _ = writeln!(&mut header);
    let _ = writeln!(&mut header, "#endif // MIMI_FFI_H");

    Ok(header)
}

impl CHeaderGenerator {
    /// Generate a C function declaration for a Mimi function exported with extern "C".
    /// Uses the same FFI contract as the binding generators so the header matches the
    /// actual C ABI (e.g. i32 is widened to int64_t, callbacks become function pointers).
    fn generate_exported_func_decl(
        &self,
        header: &mut String,
        func: &FuncDef,
    ) -> Result<(), std::fmt::Error> {
        let record_type_names: std::collections::HashSet<String> = self
            .type_defs
            .iter()
            .filter(|(_, td)| matches!(td.kind, TypeDefKind::Record(_)))
            .map(|(name, _)| name.clone())
            .collect();
        let repr_c_record_names: std::collections::HashSet<String> = self
            .type_defs
            .iter()
            .filter(|(_, td)| td.attributes.contains(&TypeAttribute::ReprC))
            .map(|(name, _)| name.clone())
            .collect();

        let extern_func = ExternFunc {
            name: func.name.clone(),
            params: func
                .params
                .iter()
                .map(|p| ExternParam {
                    name: p.name.clone(),
                    ty: p.ty.clone(),
                    cap_mode: None,
                })
                .collect(),
            ret: func.ret.clone(),
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
        };
        let contract = FfiContract::from_extern_with_caps_repr(
            &extern_func,
            &std::collections::HashSet::new(),
            &record_type_names,
            &repr_c_record_names,
        );

        let ret_type = self.contract_ret_to_c_type(&contract);
        write!(header, "{} {}(", ret_type, func.name)?;
        for (i, param) in func.params.iter().enumerate() {
            if i > 0 {
                write!(header, ", ")?;
            }
            let c_type = self.contract_arg_to_c_type(&contract, i, &param.name);
            write!(header, "{}", c_type)?;
        }
        writeln!(header, ");")?;
        Ok(())
    }
}

/// Generate C header from a list of extern functions
pub fn generate_c_header(
    extern_funcs: &[ExternFunc],
    type_defs: HashMap<String, TypeDef>,
) -> Result<String, String> {
    let generator = CHeaderGenerator::new(type_defs);
    let mut header = generator
        .generate(extern_funcs)
        .map_err(|e| format!("Failed to generate C header: {}", e))?;
    // P0-6: close the header guard here. The `#endif` was previously
    // emitted inside `generate()` which broke callers that append
    // additional declarations (exported funcs) after the fact.
    use std::fmt::Write;
    let _ = writeln!(&mut header);
    let _ = writeln!(&mut header, "#endif // MIMI_FFI_H");
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ExternParam, Type};

    #[test]
    fn test_generate_simple_header() {
        let extern_funcs = vec![ExternFunc {
            name: "add".to_string(),
            params: vec![
                ExternParam {
                    name: "a".to_string(),
                    ty: Type::Name("i32".to_string(), vec![]),
                    cap_mode: None,
                },
                ExternParam {
                    name: "b".to_string(),
                    ty: Type::Name("i32".to_string(), vec![]),
                    cap_mode: None,
                },
            ],
            ret: Some(Type::Name("i32".to_string(), vec![])),
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
        }];

        let header = generate_c_header(&extern_funcs, HashMap::new())
            .expect("src/ffi/c_header.rs:396 unwrap failed");
        assert!(header.contains("int32_t add(int32_t a, int32_t b);"));
    }

    // P0-6: the header guard `#endif` must come AFTER the function
    // declarations, not before. Regression for the user-reported
    // `mimi emit-c-headers` output that put every `extern "C"`
    // function declaration outside the include guard, where the C
    // preprocessor silently dropped them.
    #[test]
    fn test_header_endif_after_declarations() {
        let extern_funcs = vec![ExternFunc {
            name: "add".to_string(),
            params: vec![
                ExternParam {
                    name: "a".to_string(),
                    ty: Type::Name("i32".to_string(), vec![]),
                    cap_mode: None,
                },
                ExternParam {
                    name: "b".to_string(),
                    ty: Type::Name("i32".to_string(), vec![]),
                    cap_mode: None,
                },
            ],
            ret: Some(Type::Name("i32".to_string(), vec![])),
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: false,
        }];
        let header = generate_c_header(&extern_funcs, HashMap::new())
            .expect("test_header_endif_after_declarations: generate failed");
        let endif_pos = header
            .find("#endif")
            .expect("header must contain an #endif");
        let decl_pos = header
            .find("int32_t add(")
            .expect("header must contain the function declaration");
        assert!(
            decl_pos < endif_pos,
            "#endif must come after the function declaration (decl at {}, #endif at {})",
            decl_pos,
            endif_pos
        );
    }

    #[test]
    fn test_exported_funcs_inside_header_guard() {
        // The exported function path is the one that surfaced the bug
        // for the user: `generate_c_header_with_exported` previously
        // appended the export block AFTER `#endif` had been emitted.
        // We can't easily construct a full FuncDef here, but we can at
        // least verify that even with no exports the guard closes at
        // the end of the file. The "with exports" path is covered by
        // the integration test in `tests/dual_backend.rs` for the
        // emit-c-headers command.
        let header = generate_c_header(&[], HashMap::new())
            .expect("test_exported_funcs_inside_header_guard: generate failed");
        let endif_pos = header
            .rfind("#endif")
            .expect("header must contain an #endif");
        // The header must end with the #endif (or whitespace/newline after it).
        let trimmed_end = header[endif_pos..].trim_end();
        assert!(
            trimmed_end.starts_with("#endif"),
            "#endif must be the last non-whitespace token (got tail: {:?})",
            &header[endif_pos..]
        );
    }

    #[test]
    fn test_header_contains_runtime_api_declarations() {
        let header = generate_c_header(&[], HashMap::new())
            .expect("src/ffi/c_header.rs:runtime_api unwrap failed");

        // Shared handle API
        assert!(header.contains("mimi_shared_retain"));
        assert!(header.contains("mimi_shared_release"));
        assert!(header.contains("mimi_shared_get_ptr"));
        assert!(header.contains("mimi_shared_create"));

        // Value API
        assert!(header.contains("mimi_value_free"));
        assert!(header.contains("mimi_value_new_int"));
        assert!(header.contains("mimi_value_new_bool"));
        assert!(header.contains("mimi_value_new_float"));
        assert!(header.contains("mimi_value_as_int"));
        assert!(header.contains("mimi_value_as_bool"));
        assert!(header.contains("mimi_value_as_float"));

        // Capability API
        assert!(header.contains("mimi_cap_check"));
        assert!(header.contains("mimi_cap_consume"));
        assert!(header.contains("mimi_cap_register"));

        // String API
        assert!(header.contains("mimi_string_as_c_str"));
        assert!(header.contains("mimi_string_as_c_str_free"));
        assert!(header.contains("mimi_string_as_c_str_free_all"));
        assert!(header.contains("mimi_string_len"));
        assert!(header.contains("mimi_string_into_raw"));
        assert!(header.contains("mimi_string_from_raw"));
        assert!(header.contains("mimi_string_free_raw"));
        assert!(header.contains("mimi_string_free"));

        // Runtime misc API
        assert!(header.contains("mimi_runtime_set_error_handler"));
        assert!(header.contains("mimi_callback_deregister"));
        assert!(header.contains("mimi_pool_submit"));
        assert!(header.contains("mimi_pool_join_all"));
    }
}
