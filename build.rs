fn main() {
    cc::Build::new()
        .file("src/runtime/mimi_runtime.c")
        .compile("mimi_runtime");
}
