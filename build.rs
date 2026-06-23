fn main() {
    // The Mimi runtime is now implemented in Rust (src/runtime/mod.rs)
    // No C compilation needed — the runtime is compiled as part of the main crate.
    //
    // For standalone linking (codegen tests, `mimi build`), compile
    // src/runtime/standalone.rs with:
    //   rustc --edition 2021 --crate-type staticlib --crate-name mimi_runtime
    //         -o libmimi_runtime.a src/runtime/standalone.rs
}
