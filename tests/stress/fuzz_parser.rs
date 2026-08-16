// Parser fuzz smoke: malformed/truncated inputs must fail loudly, never crash.
use super::run_mimi;

fn deterministic_mutate(src: &str, seed: u64) -> String {
    let bytes = src.as_bytes().to_vec();
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as usize
    };
    let mut out = bytes.clone();
    let n = out.len().max(1);
    for _ in 0..(3 + (seed % 5) as usize) {
        let idx = next() % n;
        let byte = match next() % 6 {
            0 => b'\n',
            1 => b' ',
            2 => b'{',
            3 => b'}',
            4 => b'0',
            _ => out[idx] ^ 0x20,
        };
        out[idx] = byte;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn stress_parser_no_panic_on_malformed_inputs() {
    let base = r#"
func main() -> i32 {
    let xs = [1, 2, 3]
    println(len(xs))
    0
}
"#;
    let mut cases: Vec<String> = Vec::new();
    // Truncations at several points.
    let bytes = base.as_bytes();
    for cut in [
        0,
        1,
        5,
        10,
        bytes.len() / 3,
        bytes.len() / 2,
        bytes.len() - 1,
    ] {
        cases.push(bytes[..cut].iter().map(|&b| b as char).collect());
    }
    // Deterministic malformed mutations.
    for seed in 0..20 {
        cases.push(deterministic_mutate(base, seed));
    }
    // Random-looking garbage made only from safe ASCII punctuation.
    let mut garbage = String::new();
    for i in 0..64u8 {
        garbage.push((b'!' + (i % 90)) as char);
    }
    cases.push(garbage);

    for (i, input) in cases.iter().enumerate() {
        match run_mimi(input, &["check"]) {
            Ok(_) => {}
            Err(e) => {
                assert!(
                    !e.contains("panicked"),
                    "parser fuzz case {i}: unexpected panic: {e}"
                );
                assert!(
                    !e.contains("SIGSEGV") && !e.contains("signal"),
                    "parser fuzz case {i}: unexpected crash signal: {e}"
                );
            }
        }
    }
}
