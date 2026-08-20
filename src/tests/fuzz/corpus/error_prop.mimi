func div_safe(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { Err("div by zero") } else { Ok(a / b) }
}
func main() -> i32 {
    let r = div_safe(10, 2);
    match r { Ok(v) => v, Err(_) => -1 }
}
