//! C header generation for extern blocks and extern-exported Mimi functions.
//!
//! This module generates C header files from Mimi extern declarations,
//! allowing C code to call Mimi functions safely.

use std::collections::HashMap;
use std::fmt::Write;

use crate::ast::{ExternFunc, FuncDef, Type, TypeAttribute, TypeDef, TypeDefKind, VariantPayload};
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

        // Shared handle API (opaque integer handle IDs)
        writeln!(header, "MimiHandle mimi_shared_retain(MimiHandle handle);")?;
        writeln!(header, "void mimi_shared_release(MimiHandle handle);")?;
        writeln!(header, "void* mimi_shared_get_ptr(MimiHandle handle);")?;
        writeln!(header)?;

        // Cap API
        writeln!(header, "// Capability API")?;
        writeln!(header, "typedef int64_t MimiCap;")?;
        writeln!(header, "bool mimi_cap_check(MimiCap cap, const char* name);")?;
        writeln!(header, "bool mimi_cap_consume(MimiCap cap, const char* name);")?;
        writeln!(header)?;

        // String API
        writeln!(header, "// String API")?;
        writeln!(header, "const char* mimi_string_as_c_str(void* mimi_string);")?;
        writeln!(header, "char* mimi_string_into_raw(void* mimi_string);")?;
        writeln!(header, "void* mimi_string_from_raw(char* c_str);")?;
        writeln!(header, "void mimi_string_free_raw(char* c_str);")?;
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

        writeln!(header)?;
        writeln!(header, "#endif // MIMI_FFI_H")?;

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
                                                writeln!(header, "            {} field_{};", c_type, j)?;
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
        let record_type_names: std::collections::HashSet<String> = self.type_defs.iter()
            .filter(|(_, td)| matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
            .map(|(name, _)| name.clone())
            .collect();
        let contract = FfiContract::from_extern_with_caps(func, &std::collections::HashSet::new(), &record_type_names);

        // Return type
        let ret_type = self.contract_ret_to_c_type(&contract);

        // Function name
        write!(header, "{} {}(", ret_type, func.name)?;

        // Parameters
        for (i, param) in func.params.iter().enumerate() {
            if i > 0 {
                write!(header, ", ")?;
            }
            let c_type = self.contract_arg_to_c_type(&contract, i);
            write!(header, "{} {}", c_type, param.name)?;
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
            Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_) => {
                "MimiHandle".to_string()
            }
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
    fn contract_arg_to_c_type(&self, contract: &FfiContract, index: usize) -> String {
        if index >= contract.args.len() {
            return "void*".to_string();
        }

        match &contract.args[index] {
            FfiArgContract::Int => "int64_t".to_string(),
            FfiArgContract::Float => "double".to_string(),
            FfiArgContract::StringBorrow => "const char*".to_string(),
            FfiArgContract::StringTransfer => "char*".to_string(),
            FfiArgContract::Cap(_) => "MimiCap".to_string(),
            FfiArgContract::RawPtr(inner) | FfiArgContract::RawPtrMut(inner) => {
                format!("{}*", self.mimi_type_to_c_type(inner))
            }
            FfiArgContract::CShared(inner) | FfiArgContract::CBorrow(inner) | FfiArgContract::CBorrowMut(inner) => {
                format!("MimiHandle /* {} */", self.mimi_type_to_c_type(inner))
            }
            FfiArgContract::Json => "const char*".to_string(),
            FfiArgContract::Callback { param_types, ret_type } => {
                let ret_c = self.mimi_type_to_c_type(ret_type);
                let params_c: Vec<String> = param_types.iter()
                    .map(|t| self.mimi_type_to_c_type(t))
                    .collect();
                let params_str = if params_c.is_empty() {
                    "void".to_string()
                } else {
                    params_c.join(", ")
                };
                format!("{} (*)({})", ret_c, params_str)
            }
            FfiArgContract::Unsupported(_) => "void*".to_string(),
        }
    }

    /// Convert an FFI return contract to a C type string
    fn contract_ret_to_c_type(&self, contract: &FfiContract) -> String {
        match &contract.ret {
            crate::ffi::contract::FfiRetContract::Unit => "void".to_string(),
            crate::ffi::contract::FfiRetContract::Int => "int64_t".to_string(),
            crate::ffi::contract::FfiRetContract::Float => "double".to_string(),
            crate::ffi::contract::FfiRetContract::String => "char*".to_string(),
            crate::ffi::contract::FfiRetContract::StringOwned => "char*".to_string(),
            crate::ffi::contract::FfiRetContract::Json => "char*".to_string(),
            crate::ffi::contract::FfiRetContract::RawPtr(inner) | crate::ffi::contract::FfiRetContract::RawPtrMut(inner) => {
                format!("{}*", self.mimi_type_to_c_type(inner))
            }
            crate::ffi::contract::FfiRetContract::CShared(inner) | crate::ffi::contract::FfiRetContract::CBorrow(inner) | crate::ffi::contract::FfiRetContract::CBorrowMut(inner) => {
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
            generator.generate_exported_func_decl(&mut header, func)
                .map_err(|e| format!("Failed to generate exported func decl: {}", e))?;
        }
    }

    Ok(header)
}

impl CHeaderGenerator {
    /// Generate a C function declaration for a Mimi function exported with extern "C"
    fn generate_exported_func_decl(
        &self,
        header: &mut String,
        func: &FuncDef,
    ) -> Result<(), std::fmt::Error> {
        let ret_type = func.ret.as_ref()
            .map(|ty| self.mimi_type_to_c_type(ty))
            .unwrap_or_else(|| "void".to_string());

        write!(header, "{} {}(", ret_type, func.name)?;
        for (i, param) in func.params.iter().enumerate() {
            if i > 0 {
                write!(header, ", ")?;
            }
            let c_type = self.mimi_type_to_c_type(&param.ty);
            write!(header, "{} {}", c_type, param.name)?;
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
    generator.generate(extern_funcs).map_err(|e| format!("Failed to generate C header: {}", e))
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
        }];

        let header = generate_c_header(&extern_funcs, HashMap::new()).expect("src/ffi/c_header.rs:396 unwrap failed");
        assert!(header.contains("int64_t add(int64_t a, int64_t b);"));
    }
}
