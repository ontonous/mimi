include!("bindings/xcallbacks.rs");

fn main() {
    unsafe extern "C" fn add_one(x: i32) -> i32 { x + 1 }
    unsafe extern "C" fn add(a: i32, b: i32) -> i32 { a + b }
    unsafe extern "C" fn is_even(x: i32) -> bool { x % 2 == 0 }

    println!("Rust map_int(add_one, 5) = {}", map_int(add_one, 5));
    println!("Rust reduce_int(add, 3, 4) = {}", reduce_int(add, 3, 4));
    println!("Rust filter_int(is_even, 4) = {}", filter_int(is_even, 4));
    println!("Rust filter_int(is_even, 5) = {}", filter_int(is_even, 5));
}
