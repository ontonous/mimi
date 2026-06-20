type Opt { Some(i32) None }

func main() -> i32 {
    let x = Some(42);
    match x {
        Some(v) => v,
        None => 0,
    }
}
