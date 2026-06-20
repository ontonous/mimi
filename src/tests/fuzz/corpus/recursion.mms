func factorial(n: i32) -> i32 {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}

func main() -> i32 {
    factorial(10)
}
