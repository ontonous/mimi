//! C header generation for extern blocks.
//!
//! This module generates C header files from Mimi extern declarations,
//! allowing C code to call Mimi functions safely.

use std::collections::HashMap;
use std::fmt::Write;

use crate::ast::{ExternFunc, ExternParam, Type, TypeAttribute, TypeDef, TypeDefKind};
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

        // Forward declarations for opaque types
        writeln!(header, "// Opaque types")?;
        writeln!(header, "typedef struct MimiShared MimiShared;")?;
        writeln!(header)?;

        // MimiShared API
        writeln!(header, "// Shared handle API")?;
        writeln!(header, "MimiShared* mimi_shared_retain(MimiShared* handle);")?;
        writeln!(header, "void mimi_shared_release(MimiShared* handle);")?;
        writeln!(header, "void* mimi_shared_get_ptr(MimiShared* handle);")?;
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
                    // Generate enum with integer tag
                    writeln!(header, "typedef enum {} {{", name)?;
                    for (i, variant) in variants.iter().enumerate() {
                        writeln!(header, "    {}_{} = {},", name, variant.name, i)?;
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
        let contract = FfiContract::from_extern(func);

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
                "i32" | "i64" => "int64_t".to_string(),
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
                "MimiShared*".to_string()
            }
            Type::Cap(_) => "MimiCap".to_string(),
            Type::RawString => "char*".to_string(),
            Type::Shared(inner) | Type::LocalShared(inner) => {
                let inner_type = self.mimi_type_to_c_type(inner);
                format!("MimiShared* /* shared {} */", inner_type)
            }
            Type::Ref(inner) | Type::RefMut(inner) => {
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
            FfiArgContract::Cap => "MimiCap".to_string(),
            FfiArgContract::RawPtr(inner) | FfiArgContract::RawPtrMut(inner) => {
                format!("{}*", self.mimi_type_to_c_type(inner))
            }
            FfiArgContract::CShared(inner) | FfiArgContract::CBorrow(inner) | FfiArgContract::CBorrowMut(inner) => {
                format!("MimiShared* /* {} */", self.mimi_type_to_c_type(inner))
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
            crate::ffi::contract::FfiRetContract::RawPtr(inner) | crate::ffi::contract::FfiRetContract::RawPtrMut(inner) => {
                format!("{}*", self.mimi_type_to_c_type(inner))
            }
            crate::ffi::contract::FfiRetContract::CShared(inner) | crate::ffi::contract::FfiRetContract::CBorrow(inner) | crate::ffi::contract::FfiRetContract::CBorrowMut(inner) => {
                format!("MimiShared* /* {} */", self.mimi_type_to_c_type(inner))
            }
            crate::ffi::contract::FfiRetContract::Unsupported(_) => "void*".to_string(),
        }
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
        }];

        let header = generate_c_header(&extern_funcs, HashMap::new()).unwrap();
        assert!(header.contains("int64_t add(int64_t a, int64_t b);"));
    }
}
