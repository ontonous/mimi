func may_fail(x: i32) -> Result<i32, string> {
    if x >= 0 { Ok(x * 2) } else { Err("negative") }
}
func chain(y: i32) -> Result<i32, string> {
    let a = try may_fail(y);
    let b = try may_fail(a + 1);
    Ok(b)
}
func main() -> i32 {
    match chain(5) { Ok(v) => v, Err(_) => 0 }
}
