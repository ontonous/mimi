fn main() {
    let mut build = cc::Build::new();
    build.file("src/runtime/mimi_runtime.c");
    if cfg!(feature = "no_std") {
        build.define("MIMI_NO_STD", None);
    }
    build.compile("mimi_runtime");
}
