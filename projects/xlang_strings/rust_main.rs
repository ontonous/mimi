include!("bindings/xstrings.rs");

fn main() {
    println!("Rust greet = {}", greet("Mimi"));
    println!("Rust char_count(\"hello\") = {}", char_count("hello"));
    println!("Rust join = {}", join("Hello, ", "World"));
}
