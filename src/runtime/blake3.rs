//! Small dependency-free BLAKE3 implementation used by the standalone runtime.
//!
//! The compiler and the verifier use the `blake3` crate, but the generated
//! native program links `src/runtime/standalone.rs` with plain `rustc`.  The
//! standalone runtime must therefore be able to validate the descriptor-set
//! hash without depending on Cargo's `--extern` search path.  This is the
//! scalar, unkeyed BLAKE3 mode only; it intentionally exposes the narrow
//! incremental interface needed by the ManagedAny descriptor registry.

const OUT_LEN: usize = 32;
const BLOCK_LEN: usize = 64;
const CHUNK_LEN: usize = 1024;
const MAX_DEPTH: usize = 54;

const CHUNK_START: u8 = 1 << 0;
const CHUNK_END: u8 = 1 << 1;
const PARENT: u8 = 1 << 2;
const ROOT: u8 = 1 << 3;

const IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const MSG_SCHEDULE: [[usize; 16]; 7] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8],
    [3, 4, 10, 12, 13, 2, 7, 14, 6, 5, 9, 0, 11, 15, 8, 1],
    [10, 7, 12, 9, 14, 3, 13, 15, 4, 0, 11, 2, 5, 8, 1, 6],
    [12, 13, 9, 11, 15, 10, 14, 8, 7, 2, 5, 3, 0, 1, 6, 4],
    [9, 14, 11, 5, 8, 12, 15, 1, 13, 3, 0, 10, 2, 6, 4, 7],
    [11, 15, 5, 0, 1, 9, 8, 6, 14, 10, 2, 12, 3, 4, 7, 13],
];

#[inline(always)]
fn g(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, x: u32, y: u32) {
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(x);
    state[d] = (state[d] ^ state[a]).rotate_right(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(12);
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(y);
    state[d] = (state[d] ^ state[a]).rotate_right(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(7);
}

#[inline(always)]
fn compress_pre(
    cv: &[u32; 8],
    block: &[u8; BLOCK_LEN],
    block_len: usize,
    counter: u64,
    flags: u8,
) -> [u32; 16] {
    let mut msg = [0u32; 16];
    for (word, bytes) in msg.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    let mut state = [
        cv[0],
        cv[1],
        cv[2],
        cv[3],
        cv[4],
        cv[5],
        cv[6],
        cv[7],
        IV[0],
        IV[1],
        IV[2],
        IV[3],
        counter as u32,
        (counter >> 32) as u32,
        block_len as u32,
        flags as u32,
    ];
    for schedule in MSG_SCHEDULE {
        g(&mut state, 0, 4, 8, 12, msg[schedule[0]], msg[schedule[1]]);
        g(&mut state, 1, 5, 9, 13, msg[schedule[2]], msg[schedule[3]]);
        g(&mut state, 2, 6, 10, 14, msg[schedule[4]], msg[schedule[5]]);
        g(&mut state, 3, 7, 11, 15, msg[schedule[6]], msg[schedule[7]]);
        g(&mut state, 0, 5, 10, 15, msg[schedule[8]], msg[schedule[9]]);
        g(
            &mut state,
            1,
            6,
            11,
            12,
            msg[schedule[10]],
            msg[schedule[11]],
        );
        g(
            &mut state,
            2,
            7,
            8,
            13,
            msg[schedule[12]],
            msg[schedule[13]],
        );
        g(
            &mut state,
            3,
            4,
            9,
            14,
            msg[schedule[14]],
            msg[schedule[15]],
        );
    }
    state
}

#[inline(always)]
fn compress_cv(
    cv: &[u32; 8],
    block: &[u8; BLOCK_LEN],
    block_len: usize,
    counter: u64,
    flags: u8,
) -> [u8; OUT_LEN] {
    let state = compress_pre(cv, block, block_len, counter, flags);
    let mut out = [0u8; OUT_LEN];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&(state[i] ^ state[i + 8]).to_le_bytes());
    }
    out
}

#[inline(always)]
fn compress_root(
    cv: &[u32; 8],
    block: &[u8; BLOCK_LEN],
    block_len: usize,
    counter: u64,
    flags: u8,
) -> [u8; OUT_LEN] {
    let state = compress_pre(cv, block, block_len, counter, flags | ROOT);
    let mut out = [0u8; OUT_LEN];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&(state[i] ^ state[i + 8]).to_le_bytes());
    }
    out
}

#[inline(always)]
fn parent_cv(left: &[u8; OUT_LEN], right: &[u8; OUT_LEN]) -> [u8; OUT_LEN] {
    let mut block = [0u8; BLOCK_LEN];
    block[..OUT_LEN].copy_from_slice(left);
    block[OUT_LEN..].copy_from_slice(right);
    compress_cv(&IV, &block, BLOCK_LEN, 0, PARENT)
}

#[inline(always)]
fn parent_root(left: &[u8; OUT_LEN], right: &[u8; OUT_LEN]) -> [u8; OUT_LEN] {
    let mut block = [0u8; BLOCK_LEN];
    block[..OUT_LEN].copy_from_slice(left);
    block[OUT_LEN..].copy_from_slice(right);
    compress_root(&IV, &block, BLOCK_LEN, 0, PARENT)
}

/// A serial BLAKE3 hasher. `update` may be called with arbitrarily split
/// input; the result is identical to a single update of the concatenation.
pub(crate) struct Blake3Hasher {
    chunk_cv: [u32; 8],
    chunk_counter: u64,
    buf: [u8; BLOCK_LEN],
    buf_len: usize,
    blocks_compressed: u8,
    cv_stack: [[u8; OUT_LEN]; MAX_DEPTH],
    cv_stack_len: usize,
}

impl Blake3Hasher {
    pub(crate) fn new() -> Self {
        Self {
            chunk_cv: IV,
            chunk_counter: 0,
            buf: [0; BLOCK_LEN],
            buf_len: 0,
            blocks_compressed: 0,
            cv_stack: [[0; OUT_LEN]; MAX_DEPTH],
            cv_stack_len: 0,
        }
    }

    fn chunk_len(&self) -> usize {
        self.blocks_compressed as usize * BLOCK_LEN + self.buf_len
    }

    fn reset_chunk(&mut self) {
        self.chunk_cv = IV;
        self.buf = [0; BLOCK_LEN];
        self.buf_len = 0;
        self.blocks_compressed = 0;
    }

    fn compress_buffer(&mut self) {
        debug_assert_eq!(self.buf_len, BLOCK_LEN);
        debug_assert!(self.chunk_len() < CHUNK_LEN);
        let flags = if self.blocks_compressed == 0 {
            CHUNK_START
        } else {
            0
        };
        self.chunk_cv = compress_cv_words(
            &self.chunk_cv,
            &self.buf,
            BLOCK_LEN,
            self.chunk_counter,
            flags,
        );
        self.blocks_compressed += 1;
        self.buf = [0; BLOCK_LEN];
        self.buf_len = 0;
    }

    fn push_chunk_cv(&mut self, mut cv: [u8; OUT_LEN]) {
        // `chunk_counter` is the number of chunks already represented by the
        // stack. Merge while its trailing one bits indicate equal subtrees.
        let mut counter = self.chunk_counter;
        while counter & 1 == 1 {
            debug_assert!(self.cv_stack_len > 0);
            self.cv_stack_len -= 1;
            let left = self.cv_stack[self.cv_stack_len];
            cv = parent_cv(&left, &cv);
            counter >>= 1;
        }
        debug_assert!(self.cv_stack_len < MAX_DEPTH);
        self.cv_stack[self.cv_stack_len] = cv;
        self.cv_stack_len += 1;
    }

    fn current_chunk_cv(&self) -> [u8; OUT_LEN] {
        let block = self.buf;
        // `buf` is kept zero-filled after every compressed block, so the
        // unused tail is already canonical BLAKE3 zero padding.
        let mut flags = CHUNK_END;
        if self.blocks_compressed == 0 {
            flags |= CHUNK_START;
        }
        compress_cv(
            &self.chunk_cv,
            &block,
            self.buf_len,
            self.chunk_counter,
            flags,
        )
    }

    pub(crate) fn update(&mut self, mut input: &[u8]) {
        while !input.is_empty() {
            if self.buf_len == BLOCK_LEN {
                if self.chunk_len() == CHUNK_LEN {
                    let cv = self.current_chunk_cv();
                    self.push_chunk_cv(cv);
                    self.chunk_counter = self.chunk_counter.saturating_add(1);
                    self.reset_chunk();
                } else {
                    self.compress_buffer();
                }
                continue;
            }

            let want = BLOCK_LEN - self.buf_len;
            let take = want.min(input.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&input[..take]);
            self.buf_len += take;
            input = &input[take..];
        }
    }

    pub(crate) fn finalize(&self) -> [u8; OUT_LEN] {
        let current = self.current_chunk_cv();
        if self.cv_stack_len == 0 {
            let block = self.buf;
            let mut flags = CHUNK_END;
            if self.blocks_compressed == 0 {
                flags |= CHUNK_START;
            }
            return compress_root(
                &self.chunk_cv,
                &block,
                self.buf_len,
                self.chunk_counter,
                flags,
            );
        }

        let mut cv = current;
        let mut remaining = self.cv_stack_len;
        while remaining > 0 {
            remaining -= 1;
            let left = &self.cv_stack[remaining];
            if remaining == 0 {
                return parent_root(left, &cv);
            }
            cv = parent_cv(left, &cv);
        }
        unreachable!("non-empty descriptor hash stack did not produce a root")
    }
}

// Compression returning words is kept separate from `compress_cv` so the
// chunk state can avoid an unnecessary little-endian round trip for each
// non-final block.
#[inline(always)]
fn compress_cv_words(
    cv: &[u32; 8],
    block: &[u8; BLOCK_LEN],
    block_len: usize,
    counter: u64,
    flags: u8,
) -> [u32; 8] {
    let state = compress_pre(cv, block, block_len, counter, flags);
    [
        state[0] ^ state[8],
        state[1] ^ state[9],
        state[2] ^ state[10],
        state[3] ^ state[11],
        state[4] ^ state[12],
        state[5] ^ state[13],
        state[6] ^ state[14],
        state[7] ^ state[15],
    ]
}

#[cfg(test)]
mod tests {
    use super::Blake3Hasher;

    #[test]
    fn matches_reference_vectors() {
        let vectors: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            &[0u8; 63],
            &[0u8; 64],
            &[0u8; 65],
            &[0u8; 1023],
            &[0u8; 1024],
            &[0u8; 1025],
            &[0u8; 2048],
            &[0u8; 2049],
            &[0u8; 4097],
        ];
        for input in vectors {
            let mut hasher = Blake3Hasher::new();
            hasher.update(input);
            assert_eq!(hasher.finalize(), *::blake3::hash(input).as_bytes());
        }
    }

    #[test]
    fn split_updates_match_reference() {
        let mut hasher = Blake3Hasher::new();
        hasher.update(b"mimi-managed-any-payload-v2:");
        hasher.update(b"admitted-descriptor-set:v1\n");
        hasher.update(&[0u8; 2049]);
        let mut input = b"mimi-managed-any-payload-v2:admitted-descriptor-set:v1\n".to_vec();
        input.extend_from_slice(&[0u8; 2049]);
        assert_eq!(hasher.finalize(), *::blake3::hash(&input).as_bytes());
    }
}
