//! Cross-language binding generator smoke tests.
//!
//! These tests verify that every binding generator produces syntactically
//! reasonable output for a representative extern block, and that known
//! historical bugs do not regress.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::ast::{
        ExternFunc, ExternParam, Field, Type, TypeAttribute, TypeDef, TypeDefKind,
    };
    use crate::ffi::{
        c_header, cpp_bind, go_bind, jni_bind, node_bind, py_bind, rust_bind,
    };

    fn sample_type_defs() -> HashMap<String, TypeDef> {
        let mut map = HashMap::new();
        map.insert(
            "Point".to_string(),
            TypeDef {
                name: "Point".to_string(),
                pub_: true,
                kind: TypeDefKind::Record(vec![
                    Field {
                        name: "x".to_string(),
                        ty: Type::Name("i32".to_string(), vec![]),
                    },
                    Field {
                        name: "y".to_string(),
                        ty: Type::Name("i32".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![TypeAttribute::ReprC],
            },
        );
        map
    }

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
            ExternFunc {
                name: "point_sum".to_string(),
                params: vec![ExternParam {
                    name: "p".to_string(),
                    ty: Type::Name("Point".to_string(), vec![]),
                    cap_mode: None,
                }],
                ret: Some(Type::Name("i32".to_string(), vec![])),
                requires: None,
                ensures: None,
                variadic: false,
                no_panic: false,
            },
            ExternFunc {
                name: "apply_callback".to_string(),
                params: vec![
                    ExternParam {
                        name: "f".to_string(),
                        ty: Type::Func(
                            vec![
                                Type::Name("i32".to_string(), vec![]),
                                Type::Name("i32".to_string(), vec![]),
                            ],
                            Box::new(Type::Name("i32".to_string(), vec![])),
                        ),
                        cap_mode: None,
                    },
                    ExternParam {
                        name: "x".to_string(),
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
        ]
    }

    #[test]
    fn c_header_includes_all_runtime_declarations() {
        let header = c_header::generate_c_header(&sample_extern_funcs(), sample_type_defs()).unwrap();
        assert!(header.contains("mimi_string_free("));
        assert!(header.contains("mimi_cap_register("));
        assert!(header.contains("mimi_runtime_set_error_handler("));
        assert!(header.contains("mimi_callback_deregister("));
        assert!(header.contains("MimiErrorHandler"));
        assert!(header.contains("int64_t add(int64_t a, int64_t b)"));
        assert!(header.contains("typedef struct Point"));
        assert!(header.contains("int64_t point_sum(struct Point p)"));
    }

    #[test]
    fn rust_binding_smoke() {
        let gen = rust_bind::RustBindGenerator::new(sample_type_defs(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        assert!(out.contains("pub struct MimiPoint"));
        assert!(out.contains("pub x: i32"));
        assert!(out.contains("pub fn add("));
        assert!(out.contains("pub fn greet("));
        assert!(out.contains("pub fn point_sum(p: MimiPoint) -> c_longlong"));
        assert!(out.contains("pub fn apply_callback(f: unsafe extern \"C\" fn(c_longlong, c_longlong) -> c_longlong, x: c_longlong) -> c_longlong"));
        assert!(out.contains("extern \"C\""));
    }

    #[test]
    fn go_binding_smoke() {
        let gen = go_bind::GoBindGenerator::new(sample_type_defs(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        assert!(out.contains("package math"));
        assert!(out.contains("func add("));
        assert!(out.contains("func greet("));
        assert!(out.contains("type Point struct"));
        assert!(out.contains("func point_sum(p Point) int64"));
        assert!(out.contains("type apply_callback_f_cb func(int64, int64) int64"));
        assert!(out.contains("var apply_callback_f_cb_slot apply_callback_f_cb"));
        assert!(out.contains("//export mimi_cb_apply_callback_f"));
        assert!(out.contains("func apply_callback(f apply_callback_f_cb, x int64) int64"));
        // Regression: return type of mimi_string_free must be void, not void*.
        assert!(!out.contains("extern void* mimi_string_free"));
        assert!(out.contains("extern void mimi_string_free"));
    }

    #[test]
    fn node_binding_smoke() {
        let gen = node_bind::NodeBindGenerator::new(sample_type_defs(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        assert!(out.contains("napi_add"));
        assert!(out.contains("napi_greet"));
        assert!(out.contains("napi_point_sum"));
        assert!(out.contains("typedef struct Point"));
        assert!(out.contains("struct Point p_struct"));
        assert!(out.contains("napi_get_named_property(env, args[0], \"x\""));
        assert!(out.contains("mimi_cb_apply_callback_f_trampoline"));
        assert!(out.contains("napi_create_reference(env, args[0], 1, &mimi_cb_apply_callback_f_slot.ref)"));
        assert!(out.contains("mimi_cb_apply_callback_f_slot.env = env"));
        let dts = gen.generate_dts(&sample_extern_funcs()).unwrap();
        assert!(dts.contains("export interface Point"));
        assert!(dts.contains("export function add("));
        assert!(dts.contains("export function greet("));
        assert!(dts.contains("point_sum(p: Point): number"));
        assert!(dts.contains("apply_callback(f: (arg0: number, arg1: number) => number, x: number): number"));
    }

    #[test]
    fn cpp_binding_smoke() {
        let gen = cpp_bind::CppBindGenerator::new(sample_type_defs(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        assert!(out.contains("#include \"mimi_ffi.h\""));
        assert!(out.contains("inline int64_t add("));
        assert!(out.contains("inline MimiString greet("));
        assert!(out.contains("inline int64_t point_sum(const struct Point& p)"));
        assert!(out.contains("MimiString"));
        assert!(out.contains("std::function<int64_t(int64_t, int64_t)> apply_callback_f_cb"));
        assert!(out.contains("apply_callback_f_cb = f"));
        assert!(out.contains("mimi_cb_apply_callback_f_trampoline"));
    }

    #[test]
    fn java_binding_smoke() {
        let gen = jni_bind::JniBindGenerator::new(sample_type_defs(), "math");
        let c = gen.generate_c(&sample_extern_funcs()).unwrap();
        let java = gen.generate_java(&sample_extern_funcs()).unwrap();
        assert!(c.contains("JNIEXPORT jlong JNICALL Java_Math_add"));
        assert!(c.contains("JNIEXPORT jstring JNICALL Java_Math_greet"));
        // Regression: string args must be converted and released with the same variable name.
        assert!(c.contains("const char* name_str ="));
        assert!(c.contains("if (name_str) (*env)->ReleaseStringUTFChars(env, name, name_str)"));
        assert!(c.contains("typedef struct Point"));
        assert!(c.contains("struct Point p_struct"));
        assert!(c.contains("jclass Point_cls = (*env)->FindClass(env, \"Math$Point\")"));
        assert!(java.contains("public static native long add("));
        assert!(java.contains("public static native String greet("));
        assert!(java.contains("public static class Point"));
        assert!(java.contains("public static native long point_sum(Point p)"));
    }

    #[test]
    fn python_binding_smoke() {
        let gen = py_bind::PyBindGenerator::new(sample_type_defs(), "math");
        let out = gen.generate(&sample_extern_funcs()).unwrap();
        let pyi = gen.generate_pyi(&sample_extern_funcs()).unwrap();
        assert!(out.contains("PYBIND11_MODULE(math"));
        assert!(out.contains("m.def(\"add\""));
        assert!(out.contains("py::class_<Point>(m, \"Point\")"));
        assert!(out.contains("m.def(\"point_sum\", [](Point p) -> int64_t"));
        assert!(out.contains("thread_local static std::function<int64_t(int64_t, int64_t)> g_apply_callback_f_cb"));
        assert!(out.contains("extern \"C\" int64_t mimi_cb_apply_callback_f_trampoline"));
        assert!(out.contains("g_apply_callback_f_cb = f"));
        assert!(out.contains("mimi_cb_apply_callback_f_trampoline"));
        assert!(pyi.contains("def add("));
        assert!(pyi.contains("def greet("));
        assert!(pyi.contains("class Point:"));
        assert!(pyi.contains("def point_sum(p: Point) -> int"));
        assert!(pyi.contains("def apply_callback(f: Callable[[int, int], int], x: int) -> int"));
    }
}
