//! Cross-language binding generator smoke tests.
//!
//! These tests verify that every binding generator produces syntactically
//! reasonable output for a representative extern block, and that known
//! historical bugs do not regress.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::ast::{ExternFunc, ExternParam, Type};
    use crate::ffi::{
        c_header, cpp_bind, go_bind, jni_bind, node_bind, py_bind, rust_bind,
    };

    fn sample_extern_funcs() -> Vec<ExternFunc> {
        vec![
            ExternFunc {
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
            },
            ExternFunc {
                name: "greet".to_string(),
                params: vec![ExternParam {
                    name: "name".to_string(),
                    ty: Type::Name("string".to_string(), vec![]),
                    cap_mode: None,
                }],
                ret: Some(Type::Name("string".to_string(), vec![])),
                requires: None,
                ensures: None,
                variadic: false,
                no_panic: false,
            },
        ]
    }

    #[test]
    fn c_header_includes_all_runtime_declarations() {
        let header = c_header::generate_c_header(&sample_extern_funcs(), HashMap::new()).unwrap();
        assert!(header.contains("mimi_string_free("));
        assert!(header.contains("mimi_cap_register("));
        assert!(header.contains("mimi_runtime_set_error_handler("));
        assert!(header.contains("mimi_callback_deregister("));
        assert!(header.contains("MimiErrorHandler"));
        assert!(header.contains("int64_t add(int64_t a, int64_t b)"));
    }

    #[test]
    fn rust_binding_smoke() {
        let gen = rust_bind::RustBindGenerator::new(HashMap::new(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        assert!(out.contains("pub fn add("));
        assert!(out.contains("pub fn greet("));
        assert!(out.contains("extern \"C\""));
    }

    #[test]
    fn go_binding_smoke() {
        let gen = go_bind::GoBindGenerator::new(HashMap::new(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        assert!(out.contains("package math"));
        assert!(out.contains("func add("));
        assert!(out.contains("func greet("));
        // Regression: return type of mimi_string_free must be void, not void*.
        assert!(!out.contains("extern void* mimi_string_free"));
        assert!(out.contains("extern void mimi_string_free"));
    }

    #[test]
    fn node_binding_smoke() {
        let gen = node_bind::NodeBindGenerator::new(HashMap::new(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        assert!(out.contains("napi_add"));
        assert!(out.contains("napi_greet"));
        let dts = gen.generate_dts(&sample_extern_funcs()).unwrap();
        assert!(dts.contains("export function add("));
        assert!(dts.contains("export function greet("));
    }

    #[test]
    fn cpp_binding_smoke() {
        let gen = cpp_bind::CppBindGenerator::new(HashMap::new(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        assert!(out.contains("inline int64_t add("));
        assert!(out.contains("inline MimiString greet("));
        assert!(out.contains("MimiString"));
    }

    #[test]
    fn java_binding_smoke() {
        let gen = jni_bind::JniBindGenerator::new(HashMap::new(), "math");
        let c = gen.generate_c(&sample_extern_funcs()).unwrap();
        let java = gen.generate_java(&sample_extern_funcs()).unwrap();
        assert!(c.contains("JNIEXPORT jlong JNICALL Java_Math_add"));
        assert!(c.contains("JNIEXPORT jstring JNICALL Java_Math_greet"));
        // Regression: string args must be converted and released with the same variable name.
        assert!(c.contains("const char* name_str ="));
        assert!(c.contains("if (name_str) (*env)->ReleaseStringUTFChars(env, name, name_str)"));
        assert!(java.contains("public static native long add("));
        assert!(java.contains("public static native String greet("));
    }

    #[test]
    fn python_binding_smoke() {
        let gen = py_bind::PyBindGenerator::new(HashMap::new(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        let pyi = gen.generate_pyi(&sample_extern_funcs()).unwrap();
        assert!(out.contains("PYBIND11_MODULE(math"));
        assert!(out.contains("m.def(\"add\""));
        assert!(pyi.contains("def add("));
        assert!(pyi.contains("def greet("));
    }
}
