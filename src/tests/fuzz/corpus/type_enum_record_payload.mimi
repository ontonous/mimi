type Event {
    Click { x: i32; y: i32 }
    KeyPress { key: string }
}
func main() -> string {
    let e = Click { x: 10, y: 20 };
    match e {
        Click { x, y } => "click at " + x.to_string() + "," + y.to_string(),
        KeyPress { key } => "pressed " + key,
    }
}
