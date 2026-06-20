func main() -> i32 {
    let ok: Result<i32, string> = Ok(42);
    ok.unwrap_or(0)
}
