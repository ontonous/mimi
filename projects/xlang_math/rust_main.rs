include!("bindings/xmath.rs");

fn main() {
    println!("Rust add(2,3) = {}", add(2, 3));

    let p = MimiPoint { x: 10, y: 20 };
    println!("Rust point_sum({{10,20}}) = {}", point_sum(p));

    let q = make_point(7, 8);
    println!("Rust make_point(7,8) = {:?}", q);

    println!("Rust greet = {}", greet("Mimi"));

    unsafe extern "C" fn my_cb(a: i32, b: i32) -> i32 {
        a + b
    }
    println!("Rust apply_callback = {}", apply_callback(my_cb, 5));
}
