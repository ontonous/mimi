// Wire fuzz smoke: random/truncated binary payloads must never panic.
use mimi::component::{WireEnvelope, WireType};

fn next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 33
}

#[test]
fn stress_wire_fuzz_no_panic() {
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    let mut tested = 0u32;
    for _ in 0..1_000 {
        let len = (next(&mut state) % 96) as usize;
        let data: Vec<u8> = (0..len).map(|_| next(&mut state) as u8).collect();

        let _ = WireEnvelope::from_bytes(&data);
        let _ = WireType::I32.decode_value(&data);
        let _ = WireType::U64.decode_value(&data);
        let _ = WireType::decode_string(&data);
        let _ = WireType::decode_bytes(&data);
        let _ = WireType::Array(Box::new(WireType::U32)).decode_value(&data);
        let _ = WireType::Optional(Box::new(WireType::I64)).decode_value(&data);
        tested += 1;
    }
    // Sanity: the loop really did work, not just compile-time empty.
    assert_eq!(tested, 1_000);
}
