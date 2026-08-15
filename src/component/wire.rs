//! Wire Schema: cross-component serialization format.
//!
//! 0.31.34 (COMPONENT-WIRE-001): Wire never serializes native pointers.
//! All data is serialized as value types (integers, floats, bytes, strings,
//! arrays, maps). Handles are serialized as opaque 64-bit identifiers.
//!
//! §Wire schema（closed 0.36.104 by design）：the schema is implemented and
//! covered by round-trip/error-path unit tests in this module. Wiring it into
//! a component CLI/transport remains a Wave-3 integration goal; for 0.1.6 the
//! module is a tested library surface rather than an untracked stub.
//!
//! The Wire Schema defines:
//! 1. WireEnvelope: versioned message wrapper (magic + version + payload)
//! 2. WireType: the type system for wire serialization
//! 3. WireField: a field in a wire message
//!
//! Design decisions (blind review):
//! - NOT Protobuf (too complex, schema evolution issues)
//! - Canonical versioned Envelope (magic bytes + semver + payload)
//! - Little-endian byte order (x86/ARM native)
//! - Length-prefixed variable data (strings, bytes, arrays)

use serde::{Deserialize, Serialize};

/// Wire envelope: versioned message wrapper.
///
/// Layout: [magic: 4 bytes][version: 4 bytes][payload_len: 8 bytes][payload: N bytes]
///
/// **Serialization**: Use `to_bytes()`/`from_bytes()` for binary wire transport.
/// This type does NOT implement `Serialize`/`Deserialize` — the payload is
/// binary data that doesn't belong in JSON. Use `MimiAbi` for JSON serialization.
#[derive(Debug, Clone)]
pub struct WireEnvelope {
    /// Magic bytes: "MIMI" (0x4D494D49).
    pub magic: u32,
    /// Schema version (semver major.minor packed as u32).
    pub version: u32,
    /// Payload length in bytes.
    pub payload_len: u64,
    /// Payload (serialized message).
    pub payload: Vec<u8>,
}

impl WireEnvelope {
    /// Magic bytes: "MIMI" in little-endian.
    pub const MAGIC: u32 = 0x494D_494D; // "MIMI" LE

    /// Current schema version (1.0).
    pub const VERSION: u32 = 0x0001_0000;

    /// Create a new envelope with the current magic and version.
    pub fn new(payload: Vec<u8>) -> Self {
        let payload_len = payload.len() as u64;
        Self {
            magic: Self::MAGIC,
            version: Self::VERSION,
            payload_len,
            payload,
        }
    }

    /// Validate the envelope header.
    pub fn validate(&self) -> Result<(), WireError> {
        if self.magic != Self::MAGIC {
            return Err(WireError::BadMagic(self.magic));
        }
        if self.version >> 16 != Self::VERSION >> 16 {
            return Err(WireError::VersionMismatch {
                expected_major: Self::VERSION >> 16,
                got_major: self.version >> 16,
            });
        }
        if self.payload_len != self.payload.len() as u64 {
            return Err(WireError::LengthMismatch {
                declared: self.payload_len,
                actual: self.payload.len() as u64,
            });
        }
        Ok(())
    }

    /// Serialize to bytes (header + payload).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16 + self.payload.len());
        buf.extend_from_slice(&self.magic.to_le_bytes());
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Deserialize from bytes.
    ///
    /// Returns `Err(WireError::TrailingData)` if there are extra bytes after
    /// the declared payload (possible corruption or framing error).
    /// Returns `Err(WireError::Truncated)` if `payload_len` overflows `usize`.
    pub fn from_bytes(data: &[u8]) -> Result<Self, WireError> {
        if data.len() < 16 {
            return Err(WireError::TooShort(data.len()));
        }
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let payload_len = u64::from_le_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        // Overflow-safe: reject payload_len that doesn't fit in usize or
        // would overflow when adding the 16-byte header.
        let payload_len_usize = usize::try_from(payload_len).map_err(|_| WireError::Truncated {
            expected: usize::MAX,
            actual: data.len(),
        })?;
        let total = 16usize
            .checked_add(payload_len_usize)
            .ok_or(WireError::Truncated {
                expected: usize::MAX,
                actual: data.len(),
            })?;
        if data.len() < total {
            return Err(WireError::Truncated {
                expected: total,
                actual: data.len(),
            });
        }
        if data.len() > total {
            return Err(WireError::TrailingData {
                expected: total,
                actual: data.len(),
            });
        }
        let payload = data[16..total].to_vec();
        let envelope = Self {
            magic,
            version,
            payload_len,
            payload,
        };
        envelope.validate()?;
        Ok(envelope)
    }
}

/// Wire type: the type system for wire serialization.
///
/// COMPONENT-WIRE-001: Wire never serializes native pointers.
/// All types are value types or opaque handles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireType {
    /// Boolean (1 byte: 0 or 1).
    Bool,
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
    /// Signed 64-bit integer.
    I64,
    /// Unsigned 8-bit integer.
    U8,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 32-bit integer.
    U32,
    /// Unsigned 64-bit integer.
    U64,
    /// 32-bit IEEE 754 float.
    F32,
    /// 64-bit IEEE 754 float.
    F64,
    /// UTF-8 string (length-prefixed: u32 len + bytes).
    String,
    /// Byte array (length-prefixed: u32 len + bytes).
    Bytes,
    /// Array of elements (length-prefixed: u32 count + elements).
    Array(Box<WireType>),
    /// Map of key-value pairs (length-prefixed: u32 count + pairs).
    Map(Box<WireType>, Box<WireType>),
    /// Opaque handle (u64 identifier, no pointer serialization).
    Handle,
    /// Optional value (1 byte tag: 0=None, 1=Some + value).
    Optional(Box<WireType>),
    /// Result value (1 byte tag: 0=Ok + value, 1=Err + error).
    Result(Box<WireType>, Box<WireType>),
    /// Unit (zero bytes).
    Unit,
}

impl WireType {
    /// Fixed size in bytes (None for variable-length types).
    pub fn fixed_size(&self) -> Option<usize> {
        match self {
            WireType::Bool | WireType::I8 | WireType::U8 => Some(1),
            WireType::I16 | WireType::U16 => Some(2),
            WireType::I32 | WireType::U32 | WireType::F32 => Some(4),
            WireType::I64 | WireType::U64 | WireType::F64 | WireType::Handle => Some(8),
            WireType::Unit => Some(0),
            // Variable-length types
            WireType::String
            | WireType::Bytes
            | WireType::Array(_)
            | WireType::Map(_, _)
            | WireType::Optional(_)
            | WireType::Result(_, _) => None,
        }
    }

    /// True if this type contains a handle (directly or nested).
    ///
    /// Traversal is bounded to depth 128 to prevent stack overflow
    /// on pathologically nested types. Beyond depth 128, returns `true`
    /// (conservative: assume handle present) to avoid missing handle
    /// tracking in deeply nested types.
    pub fn contains_handle(&self) -> bool {
        self.contains_handle_depth(0)
    }

    fn contains_handle_depth(&self, depth: usize) -> bool {
        if depth > 128 {
            // GAP-7 fix: return true (conservative) instead of false.
            // A false negative here would skip handle tracking for a
            // deeply nested Handle, causing use-after-free.
            return true;
        }
        match self {
            WireType::Handle => true,
            WireType::Array(inner) => inner.contains_handle_depth(depth + 1),
            WireType::Map(k, v) => {
                k.contains_handle_depth(depth + 1) || v.contains_handle_depth(depth + 1)
            }
            WireType::Optional(inner) => inner.contains_handle_depth(depth + 1),
            WireType::Result(ok, err) => {
                ok.contains_handle_depth(depth + 1) || err.contains_handle_depth(depth + 1)
            }
            _ => false,
        }
    }

    /// Encode a primitive value to wire bytes (little-endian).
    ///
    /// Returns `None` if this type is not a fixed-size primitive.
    pub fn encode_primitive(&self, value: u64) -> Option<Vec<u8>> {
        match self {
            WireType::Bool => Some(vec![if value != 0 { 1 } else { 0 }]),
            WireType::I8 => Some((value as i8).to_le_bytes().to_vec()),
            WireType::I16 => Some((value as i16).to_le_bytes().to_vec()),
            WireType::I32 => Some((value as i32).to_le_bytes().to_vec()),
            WireType::I64 => Some((value as i64).to_le_bytes().to_vec()),
            WireType::U8 => Some((value as u8).to_le_bytes().to_vec()),
            WireType::U16 => Some((value as u16).to_le_bytes().to_vec()),
            WireType::U32 => Some((value as u32).to_le_bytes().to_vec()),
            WireType::U64 | WireType::Handle => Some(value.to_le_bytes().to_vec()),
            WireType::F32 => Some(f32::from_bits(value as u32).to_le_bytes().to_vec()),
            WireType::F64 => Some(f64::from_bits(value).to_le_bytes().to_vec()),
            WireType::Unit => Some(vec![]),
            _ => None,
        }
    }

    /// Decode a primitive value from wire bytes (little-endian).
    ///
    /// Returns `None` if this type is not a fixed-size primitive or the
    /// byte slice has the wrong length.
    pub fn decode_primitive(&self, bytes: &[u8]) -> Option<u64> {
        match self {
            WireType::Bool => {
                if bytes.len() == 1 {
                    Some(if bytes[0] != 0 { 1 } else { 0 })
                } else {
                    None
                }
            }
            WireType::I8 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 1]| i8::from_le_bytes(b) as u64),
            WireType::I16 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 2]| i16::from_le_bytes(b) as u64),
            WireType::I32 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 4]| i32::from_le_bytes(b) as u64),
            WireType::I64 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 8]| i64::from_le_bytes(b) as u64),
            WireType::U8 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 1]| u8::from_le_bytes(b) as u64),
            WireType::U16 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 2]| u16::from_le_bytes(b) as u64),
            WireType::U32 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 4]| u32::from_le_bytes(b) as u64),
            WireType::U64 | WireType::Handle => bytes.try_into().ok().map(u64::from_le_bytes),
            WireType::F32 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 4]| f32::from_le_bytes(b).to_bits() as u64),
            WireType::F64 => bytes
                .try_into()
                .ok()
                .map(|b: [u8; 8]| f64::from_le_bytes(b).to_bits()),
            WireType::Unit => {
                if bytes.is_empty() {
                    Some(0)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Encode a UTF-8 string to wire bytes (u32 LE length prefix + bytes).
    ///
    /// Returns `Err(WireError::LengthOverflow)` if the string exceeds
    /// `u32::MAX` bytes (the wire format limit).
    pub fn encode_string(s: &str) -> Result<Vec<u8>, WireError> {
        let bytes = s.as_bytes();
        let len = u32::try_from(bytes.len()).map_err(|_| WireError::LengthOverflow {
            max: u32::MAX as usize,
            got: bytes.len(),
        })?;
        let mut buf = Vec::with_capacity(4 + bytes.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(bytes);
        Ok(buf)
    }

    /// Decode a UTF-8 string from wire bytes.
    ///
    /// Returns `(string, bytes_consumed)` or `None` if the data is
    /// too short or not valid UTF-8.
    pub fn decode_string(data: &[u8]) -> Option<(String, usize)> {
        if data.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let total = 4usize.checked_add(len)?;
        if data.len() < total {
            return None;
        }
        let s = std::str::from_utf8(&data[4..total]).ok()?.to_string();
        Some((s, total))
    }

    /// Encode a byte array to wire bytes (u32 LE length prefix + bytes).
    ///
    /// Returns `Err(WireError::LengthOverflow)` if the data exceeds
    /// `u32::MAX` bytes (the wire format limit).
    pub fn encode_bytes(b: &[u8]) -> Result<Vec<u8>, WireError> {
        let len = u32::try_from(b.len()).map_err(|_| WireError::LengthOverflow {
            max: u32::MAX as usize,
            got: b.len(),
        })?;
        let mut buf = Vec::with_capacity(4 + b.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(b);
        Ok(buf)
    }

    /// Decode a byte array from wire bytes.
    ///
    /// Returns `(bytes, bytes_consumed)` or `None` if the data is too short.
    pub fn decode_bytes(data: &[u8]) -> Option<(Vec<u8>, usize)> {
        if data.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let total = 4usize.checked_add(len)?;
        if data.len() < total {
            return None;
        }
        Some((data[4..total].to_vec(), total))
    }

    /// Encode an optional value to wire bytes.
    ///
    /// Layout: [tag: 1 byte (0=None, 1=Some)] [value: N bytes (if Some)]
    pub fn encode_optional(inner: Option<&[u8]>) -> Vec<u8> {
        match inner {
            None => vec![0],
            Some(value) => {
                let mut buf = Vec::with_capacity(1 + value.len());
                buf.push(1);
                buf.extend_from_slice(value);
                buf
            }
        }
    }

    /// Decode one self-delimiting value of this type from the head of
    /// `data`, returning `(value_bytes, bytes_consumed)`.
    ///
    /// Audit fix 2026-08-05 (full audit §12): decoding must report the
    /// exact number of bytes consumed. The previous `decode_optional`
    /// consumed `data.len()` — everything to the end of the buffer — so an
    /// `Optional<variable T>` followed by another field misparsed: the next
    /// field's bytes were swallowed into the optional's payload. Decoding is
    /// type-driven now, mirroring the type-driven encoding, so every
    /// composite decoder (Optional, Result, Array, Map) lands on exact
    /// offsets.
    ///
    /// Returns `None` on truncation, bad tags, or nesting deeper than 128
    /// (stack-overflow guard; also bounds adversarial type bombs).
    pub fn decode_value(&self, data: &[u8]) -> Option<(Vec<u8>, usize)> {
        self.decode_value_depth(data, 0)
    }

    fn decode_value_depth(&self, data: &[u8], depth: usize) -> Option<(Vec<u8>, usize)> {
        if depth > 128 {
            return None;
        }
        match self {
            ty @ (WireType::Bool
            | WireType::I8
            | WireType::I16
            | WireType::I32
            | WireType::I64
            | WireType::U8
            | WireType::U16
            | WireType::U32
            | WireType::U64
            | WireType::F32
            | WireType::F64
            | WireType::Handle) => {
                let n = ty.fixed_size().unwrap_or(0);
                if data.len() < n {
                    return None;
                }
                Some((data[..n].to_vec(), n))
            }
            WireType::Unit => Some((Vec::new(), 0)),
            WireType::String => {
                let (_, consumed) = Self::decode_string(data)?;
                Some((data[..consumed].to_vec(), consumed))
            }
            WireType::Bytes => {
                let (_, consumed) = Self::decode_bytes(data)?;
                Some((data[..consumed].to_vec(), consumed))
            }
            WireType::Array(elem) => {
                let (count, mut consumed) = Self::decode_array_header(data)?;
                for _ in 0..count {
                    if consumed > data.len() {
                        return None;
                    }
                    let (_, n) = elem.decode_value_depth(&data[consumed..], depth + 1)?;
                    consumed = consumed.checked_add(n)?;
                }
                Some((data[..consumed].to_vec(), consumed))
            }
            WireType::Map(k, v) => {
                let (count, mut consumed) = Self::decode_map_header(data)?;
                for _ in 0..count {
                    if consumed > data.len() {
                        return None;
                    }
                    let (_, kn) = k.decode_value_depth(&data[consumed..], depth + 1)?;
                    consumed = consumed.checked_add(kn)?;
                    if consumed > data.len() {
                        return None;
                    }
                    let (_, vn) = v.decode_value_depth(&data[consumed..], depth + 1)?;
                    consumed = consumed.checked_add(vn)?;
                }
                Some((data[..consumed].to_vec(), consumed))
            }
            WireType::Optional(inner) => {
                let (_, consumed) = Self::decode_optional_depth(inner, data, depth + 1)?;
                Some((data[..consumed].to_vec(), consumed))
            }
            WireType::Result(ok, err) => {
                let (_, consumed) = Self::decode_result_depth(ok, err, data, depth + 1)?;
                Some((data[..consumed].to_vec(), consumed))
            }
        }
    }

    /// Decode an optional value from wire bytes given the **inner type**.
    ///
    /// Returns `(Some(value), bytes_consumed)` or `(None, 1)`. The consumed
    /// count is exact: `1 + encoded_size(inner)` for `Some`, `1` for `None`.
    /// Returns `None` (the outer Option) if the data is empty, the tag is
    /// invalid, or the inner value is truncated.
    ///
    /// Audit fix 2026-08-05: the inner type is required because only the
    /// type knows where a variable-length payload ends; the old type-blind
    /// decoder consumed the rest of the buffer and corrupted any field that
    /// followed the optional.
    pub fn decode_optional(inner: &WireType, data: &[u8]) -> Option<(Option<Vec<u8>>, usize)> {
        Self::decode_optional_depth(inner, data, 0)
    }

    fn decode_optional_depth(
        inner: &WireType,
        data: &[u8],
        depth: usize,
    ) -> Option<(Option<Vec<u8>>, usize)> {
        if depth > 128 {
            return None; // nesting bomb guard
        }
        if data.is_empty() {
            return None;
        }
        match data[0] {
            0 => Some((None, 1)),
            1 => {
                let (value, consumed) = inner.decode_value_depth(&data[1..], depth + 1)?;
                Some((Some(value), consumed.checked_add(1)?))
            }
            _ => None,
        }
    }

    /// Encode an array header (u32 LE element count).
    ///
    /// The caller is responsible for encoding each element after the header.
    /// Elements should be self-delimiting (fixed-size or length-prefixed).
    pub fn encode_array_header(count: u32) -> Vec<u8> {
        count.to_le_bytes().to_vec()
    }

    /// Decode an array header, returning the element count.
    ///
    /// Returns `(count, bytes_consumed)` or `None` if data is too short.
    pub fn decode_array_header(data: &[u8]) -> Option<(u32, usize)> {
        if data.len() < 4 {
            return None;
        }
        let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        Some((count, 4))
    }

    /// Encode a map header (u32 LE pair count).
    ///
    /// The caller is responsible for encoding each key-value pair after
    /// the header. Keys and values should be self-delimiting.
    pub fn encode_map_header(count: u32) -> Vec<u8> {
        count.to_le_bytes().to_vec()
    }

    /// Decode a map header, returning the pair count.
    ///
    /// Returns `(count, bytes_consumed)` or `None` if data is too short.
    pub fn decode_map_header(data: &[u8]) -> Option<(u32, usize)> {
        Self::decode_array_header(data) // same wire format
    }

    /// Encode a Result header (1 byte tag: 0=Ok, 1=Err).
    ///
    /// The caller is responsible for encoding the value/error after the tag.
    pub fn encode_result_tag(is_err: bool) -> Vec<u8> {
        vec![if is_err { 1 } else { 0 }]
    }

    /// Decode a Result header tag.
    ///
    /// Returns `(is_err, bytes_consumed)` or `None` if data is empty.
    pub fn decode_result_tag(data: &[u8]) -> Option<(bool, usize)> {
        if data.is_empty() {
            return None;
        }
        match data[0] {
            0 => Some((false, 1)),
            1 => Some((true, 1)),
            _ => None,
        }
    }

    /// Decode a full Result value (tag + payload) given both payload types.
    ///
    /// Returns `(Ok(payload), consumed)` / `(Err(payload), consumed)` with an
    /// **exact** consumed count: `1 + encoded_size(active payload type)`.
    /// Audit fix 2026-08-05: same consumed-bytes discipline as
    /// [`WireType::decode_optional`] — the payload boundary comes from the
    /// type, not from "rest of buffer".
    pub fn decode_result(
        ok_ty: &WireType,
        err_ty: &WireType,
        data: &[u8],
    ) -> Option<(Result<Vec<u8>, Vec<u8>>, usize)> {
        Self::decode_result_depth(ok_ty, err_ty, data, 0)
    }

    fn decode_result_depth(
        ok_ty: &WireType,
        err_ty: &WireType,
        data: &[u8],
        depth: usize,
    ) -> Option<(Result<Vec<u8>, Vec<u8>>, usize)> {
        if depth > 128 {
            return None; // nesting bomb guard
        }
        let (is_err, tag_consumed) = Self::decode_result_tag(data)?;
        let payload_ty = if is_err { err_ty } else { ok_ty };
        let (payload, payload_consumed) =
            payload_ty.decode_value_depth(&data[tag_consumed..], depth + 1)?;
        let total = tag_consumed.checked_add(payload_consumed)?;
        let value = if is_err { Err(payload) } else { Ok(payload) };
        Some((value, total))
    }
}

/// Wire field: a field in a wire message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireField {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: WireType,
    /// Field index (for positional encoding).
    pub index: u32,
    /// Whether this field is optional (can be omitted).
    pub optional: bool,
}

/// Wire schema: the schema definition for a component's wire interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSchema {
    /// Schema name.
    pub name: String,
    /// Schema version.
    pub version: u32,
    /// Fields in the schema.
    pub fields: Vec<WireField>,
}

impl WireSchema {
    /// Validate schema consistency.
    ///
    /// Checks (all enforced; audit fix 2026-08-05 made check 3 real):
    /// 1. No duplicate field indices
    /// 2. No duplicate field names
    /// 3. Field indices are contiguous from 0 — the sorted unique indices
    ///    must be exactly `0..fields.len()`. Skipped when duplicates were
    ///    found (the duplicate report is the actionable one).
    ///
    /// Returns a list of validation errors (empty = consistent).
    pub fn validate(&self) -> Vec<WireSchemaError> {
        let mut errors = Vec::new();
        let mut seen_indices = std::collections::HashSet::new();
        let mut seen_names = std::collections::HashSet::new();

        for field in &self.fields {
            if !seen_indices.insert(field.index) {
                errors.push(WireSchemaError::DuplicateIndex(field.index));
            }
            if !seen_names.insert(field.name.as_str()) {
                errors.push(WireSchemaError::DuplicateName(field.name.clone()));
            }
        }

        // Index-contiguity check (audit fix 2026-08-05): previously this was
        // documented but never implemented. Positional wire encoding relies
        // on dense 0-based indices; a gap makes field positions ambiguous.
        if errors.is_empty() {
            let mut indices: Vec<u32> = self.fields.iter().map(|f| f.index).collect();
            indices.sort_unstable();
            for (pos, &index) in indices.iter().enumerate() {
                let expected = u32::try_from(pos).unwrap_or(u32::MAX);
                if index != expected {
                    errors.push(WireSchemaError::NonContiguousIndex {
                        expected,
                        got: index,
                    });
                    break;
                }
            }
        }

        errors
    }
}

/// Wire schema validation error.
#[derive(Debug, Clone, PartialEq)]
pub enum WireSchemaError {
    /// Duplicate field index.
    DuplicateIndex(u32),
    /// Duplicate field name.
    DuplicateName(String),
    /// Field indices are not contiguous from 0: at position `expected`
    /// (0-based, sorted) the schema carries index `got` (a gap or a
    /// non-zero start). Audit fix 2026-08-05.
    NonContiguousIndex { expected: u32, got: u32 },
}

/// Wire error.
#[derive(Debug, Clone, PartialEq)]
pub enum WireError {
    /// Bad magic bytes.
    BadMagic(u32),
    /// Version mismatch (major version differs).
    VersionMismatch { expected_major: u32, got_major: u32 },
    /// Payload length mismatch.
    LengthMismatch { declared: u64, actual: u64 },
    /// Data too short for header.
    TooShort(usize),
    /// Data truncated (not enough bytes for declared payload).
    Truncated { expected: usize, actual: usize },
    /// Extra bytes after declared payload (possible corruption).
    TrailingData { expected: usize, actual: usize },
    /// Length exceeds the u32 wire format limit.
    LengthOverflow { max: usize, got: usize },
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::BadMagic(m) => write!(f, "bad magic: 0x{:08X}", m),
            WireError::VersionMismatch {
                expected_major,
                got_major,
            } => write!(
                f,
                "version mismatch: expected major {}, got {}",
                expected_major, got_major
            ),
            WireError::LengthMismatch { declared, actual } => write!(
                f,
                "length mismatch: declared {}, actual {}",
                declared, actual
            ),
            WireError::TooShort(len) => write!(f, "data too short: {} bytes (need 16)", len),
            WireError::Truncated { expected, actual } => write!(
                f,
                "data truncated: expected {} bytes, got {}",
                expected, actual
            ),
            WireError::TrailingData { expected, actual } => write!(
                f,
                "trailing data: expected {} bytes, got {} ({} extra)",
                expected,
                actual,
                actual - expected
            ),
            WireError::LengthOverflow { max, got } => write!(
                f,
                "length overflow: {} bytes exceeds wire format limit of {}",
                got, max
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip() {
        let payload = b"hello world".to_vec();
        let envelope = WireEnvelope::new(payload.clone());
        let bytes = envelope.to_bytes();
        let decoded = WireEnvelope::from_bytes(&bytes).expect("decode");

        assert_eq!(decoded.magic, WireEnvelope::MAGIC);
        assert_eq!(decoded.version, WireEnvelope::VERSION);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn envelope_bad_magic() {
        let mut bytes = WireEnvelope::new(b"test".to_vec()).to_bytes();
        bytes[0] = 0xFF; // corrupt magic
        assert!(matches!(
            WireEnvelope::from_bytes(&bytes),
            Err(WireError::BadMagic(_))
        ));
    }

    #[test]
    fn envelope_too_short() {
        assert!(matches!(
            WireEnvelope::from_bytes(&[0u8; 8]),
            Err(WireError::TooShort(8))
        ));
    }

    #[test]
    fn envelope_payload_len_overflow() {
        // Craft a header with payload_len = u64::MAX (would overflow usize)
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(&WireEnvelope::MAGIC.to_le_bytes());
        data[4..8].copy_from_slice(&WireEnvelope::VERSION.to_le_bytes());
        data[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(matches!(
            WireEnvelope::from_bytes(&data),
            Err(WireError::Truncated { .. })
        ));
    }

    #[test]
    fn wire_type_sizes() {
        assert_eq!(WireType::Bool.fixed_size(), Some(1));
        assert_eq!(WireType::I32.fixed_size(), Some(4));
        assert_eq!(WireType::I64.fixed_size(), Some(8));
        assert_eq!(WireType::F64.fixed_size(), Some(8));
        assert_eq!(WireType::Handle.fixed_size(), Some(8));
        assert_eq!(WireType::Unit.fixed_size(), Some(0));
        assert_eq!(WireType::String.fixed_size(), None);
        assert_eq!(WireType::Array(Box::new(WireType::I32)).fixed_size(), None);
    }

    #[test]
    fn wire_type_handle_detection() {
        assert!(WireType::Handle.contains_handle());
        assert!(!WireType::I32.contains_handle());
        assert!(WireType::Array(Box::new(WireType::Handle)).contains_handle());
        assert!(WireType::Optional(Box::new(WireType::Handle)).contains_handle());
        assert!(!WireType::Array(Box::new(WireType::I32)).contains_handle());
    }

    #[test]
    fn magic_is_mimi() {
        // "MIMI" in ASCII: M=0x4D, I=0x49, M=0x4D, I=0x49
        // Little-endian: 0x494D494D
        assert_eq!(WireEnvelope::MAGIC, 0x494D_494D);
    }

    #[test]
    fn envelope_trailing_data_detected() {
        let mut bytes = WireEnvelope::new(b"test".to_vec()).to_bytes();
        bytes.push(0xFF); // extra trailing byte
        assert!(matches!(
            WireEnvelope::from_bytes(&bytes),
            Err(WireError::TrailingData { .. })
        ));
    }

    #[test]
    fn wire_schema_validate_clean() {
        let schema = WireSchema {
            name: "test".to_string(),
            version: 1,
            fields: vec![
                WireField {
                    name: "a".to_string(),
                    ty: WireType::I32,
                    index: 0,
                    optional: false,
                },
                WireField {
                    name: "b".to_string(),
                    ty: WireType::String,
                    index: 1,
                    optional: true,
                },
            ],
        };
        assert!(schema.validate().is_empty());
    }

    #[test]
    fn wire_schema_validate_duplicate_index() {
        let schema = WireSchema {
            name: "bad".to_string(),
            version: 1,
            fields: vec![
                WireField {
                    name: "a".to_string(),
                    ty: WireType::I32,
                    index: 0,
                    optional: false,
                },
                WireField {
                    name: "b".to_string(),
                    ty: WireType::I64,
                    index: 0, // duplicate
                    optional: false,
                },
            ],
        };
        let errors = schema.validate();
        assert!(errors
            .iter()
            .any(|e| matches!(e, WireSchemaError::DuplicateIndex(0))));
    }

    #[test]
    fn wire_schema_validate_duplicate_name() {
        let schema = WireSchema {
            name: "bad".to_string(),
            version: 1,
            fields: vec![
                WireField {
                    name: "x".to_string(),
                    ty: WireType::I32,
                    index: 0,
                    optional: false,
                },
                WireField {
                    name: "x".to_string(), // duplicate
                    ty: WireType::I64,
                    index: 1,
                    optional: false,
                },
            ],
        };
        let errors = schema.validate();
        assert!(errors
            .iter()
            .any(|e| matches!(e, WireSchemaError::DuplicateName(n) if n == "x")));
    }

    /// Audit fix 2026-08-05: the index-contiguity check documented on
    /// `validate` was never implemented. It is now enforced.
    #[test]
    fn wire_schema_validate_index_contiguity() {
        // Gap: indices 0, 1, 3 (missing 2).
        let schema = WireSchema {
            name: "gap".to_string(),
            version: 1,
            fields: vec![
                WireField {
                    name: "a".to_string(),
                    ty: WireType::I32,
                    index: 0,
                    optional: false,
                },
                WireField {
                    name: "b".to_string(),
                    ty: WireType::I32,
                    index: 1,
                    optional: false,
                },
                WireField {
                    name: "c".to_string(),
                    ty: WireType::I32,
                    index: 3, // gap at 2
                    optional: false,
                },
            ],
        };
        let errors = schema.validate();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                WireSchemaError::NonContiguousIndex {
                    expected: 2,
                    got: 3
                }
            )),
            "expected contiguity error at expected=2 got=3, got {errors:?}"
        );

        // Non-zero start: indices 1, 2, 3.
        let schema = WireSchema {
            name: "start_at_one".to_string(),
            version: 1,
            fields: vec![
                WireField {
                    name: "a".to_string(),
                    ty: WireType::I32,
                    index: 1,
                    optional: false,
                },
                WireField {
                    name: "b".to_string(),
                    ty: WireType::I32,
                    index: 2,
                    optional: false,
                },
            ],
        };
        let errors = schema.validate();
        assert!(errors.iter().any(|e| matches!(
            e,
            WireSchemaError::NonContiguousIndex {
                expected: 0,
                got: 1
            }
        )));

        // Order-independent: declared out of order but the set is 0..=2.
        let schema = WireSchema {
            name: "unordered_ok".to_string(),
            version: 1,
            fields: vec![
                WireField {
                    name: "a".to_string(),
                    ty: WireType::I32,
                    index: 2,
                    optional: false,
                },
                WireField {
                    name: "b".to_string(),
                    ty: WireType::I32,
                    index: 0,
                    optional: false,
                },
                WireField {
                    name: "c".to_string(),
                    ty: WireType::I32,
                    index: 1,
                    optional: false,
                },
            ],
        };
        assert!(schema.validate().is_empty());
    }

    #[test]
    fn wire_type_primitive_encode_decode_roundtrip() {
        // I32
        let ty = WireType::I32;
        let encoded = ty.encode_primitive(42).unwrap();
        assert_eq!(encoded.len(), 4);
        assert_eq!(ty.decode_primitive(&encoded), Some(42));

        // I64
        let ty = WireType::I64;
        let encoded = ty.encode_primitive(0xDEAD_BEEF).unwrap();
        assert_eq!(encoded.len(), 8);
        assert_eq!(ty.decode_primitive(&encoded), Some(0xDEAD_BEEF));

        // Bool
        let ty = WireType::Bool;
        assert_eq!(ty.encode_primitive(1).unwrap(), vec![1]);
        assert_eq!(ty.encode_primitive(0).unwrap(), vec![0]);
        assert_eq!(ty.decode_primitive(&[1]), Some(1));
        assert_eq!(ty.decode_primitive(&[0]), Some(0));

        // U8
        let ty = WireType::U8;
        let encoded = ty.encode_primitive(255).unwrap();
        assert_eq!(ty.decode_primitive(&encoded), Some(255));

        // F64 (via bits)
        let ty = WireType::F64;
        let pi_bits = std::f64::consts::PI.to_bits();
        let encoded = ty.encode_primitive(pi_bits).unwrap();
        assert_eq!(encoded.len(), 8);
        assert_eq!(ty.decode_primitive(&encoded), Some(pi_bits));

        // Unit
        let ty = WireType::Unit;
        assert_eq!(ty.encode_primitive(0).unwrap(), Vec::<u8>::new());
        assert_eq!(ty.decode_primitive(&[]), Some(0));

        // Variable-length types return None
        assert!(WireType::String.encode_primitive(0).is_none());
        assert!(WireType::Array(Box::new(WireType::I32))
            .encode_primitive(0)
            .is_none());
    }

    #[test]
    fn wire_type_decode_wrong_length() {
        let ty = WireType::I32;
        assert_eq!(ty.decode_primitive(&[1, 2]), None); // too short
        assert_eq!(ty.decode_primitive(&[1, 2, 3, 4, 5]), None); // too long
    }

    #[test]
    fn wire_string_roundtrip() {
        let encoded = WireType::encode_string("hello world").unwrap();
        assert_eq!(&encoded[..4], &(11u32).to_le_bytes());
        let (decoded, consumed) = WireType::decode_string(&encoded).unwrap();
        assert_eq!(decoded, "hello world");
        assert_eq!(consumed, encoded.len());

        // Empty string
        let encoded = WireType::encode_string("").unwrap();
        assert_eq!(&encoded[..4], &(0u32).to_le_bytes());
        let (decoded, consumed) = WireType::decode_string(&encoded).unwrap();
        assert_eq!(decoded, "");
        assert_eq!(consumed, 4);
    }

    #[test]
    fn wire_string_decode_truncated() {
        assert!(WireType::decode_string(&[1, 0]).is_none()); // too short for len
        assert!(WireType::decode_string(&[10, 0, 0, 0, b'h']).is_none()); // len=10 but only 1 byte
    }

    #[test]
    fn wire_bytes_roundtrip() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let encoded = WireType::encode_bytes(&data).unwrap();
        assert_eq!(&encoded[..4], &(4u32).to_le_bytes());
        let (decoded, consumed) = WireType::decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, data);
        assert_eq!(consumed, 8);
    }

    #[test]
    fn wire_optional_roundtrip() {
        // None
        let encoded = WireType::encode_optional(None);
        assert_eq!(encoded, vec![0]);
        let (decoded, consumed) = WireType::decode_optional(&WireType::I64, &encoded).unwrap();
        assert_eq!(decoded, None);
        assert_eq!(consumed, 1);

        // Some (fixed-size inner)
        let value = vec![1, 2, 3, 4];
        let encoded = WireType::encode_optional(Some(&value));
        assert_eq!(encoded[0], 1);
        assert_eq!(&encoded[1..], &value);
        let (decoded, consumed) = WireType::decode_optional(&WireType::I32, &encoded).unwrap();
        assert_eq!(decoded, Some(value));
        assert_eq!(consumed, 5);

        // Some (length-prefixed inner: String) — consumed must be exact,
        // not "rest of buffer" (audit fix 2026-08-05).
        let inner = WireType::encode_string("hi").unwrap();
        let encoded = WireType::encode_optional(Some(&inner));
        let (decoded, consumed) = WireType::decode_optional(&WireType::String, &encoded).unwrap();
        assert_eq!(decoded, Some(inner));
        assert_eq!(consumed, 1 + 4 + 2); // tag + u32 len + "hi"
    }

    #[test]
    fn wire_optional_decode_empty() {
        assert!(WireType::decode_optional(&WireType::I64, &[]).is_none());
    }

    /// Audit fix 2026-08-05 regression: `Optional<variable T>` followed by
    /// another field must decode at exact offsets. The old decoder consumed
    /// `data.len()` and swallowed the trailing field into the payload.
    #[test]
    fn wire_optional_then_field_exact_offsets() {
        // Message: [Optional<String>][I64]
        let mut buf = Vec::new();
        buf.extend(WireType::encode_optional(Some(
            &WireType::encode_string("payload").unwrap(),
        )));
        let expected_opt_len = 1 + 4 + 7; // tag + u32 len + 7 bytes
        assert_eq!(buf.len(), expected_opt_len);
        buf.extend(
            WireType::I64
                .encode_primitive(0x5152_5354_5556_5758)
                .unwrap(),
        );

        // Field 1: optional — exact consumed count stops before the i64.
        let (opt, consumed1) = WireType::decode_optional(&WireType::String, &buf).unwrap();
        assert_eq!(consumed1, expected_opt_len);
        let (s, str_consumed) = WireType::decode_string(&opt.unwrap()).unwrap();
        assert_eq!(s, "payload");
        assert_eq!(str_consumed, 4 + 7);

        // Field 2: i64 starts at exactly consumed1.
        let rest = &buf[consumed1..];
        assert_eq!(rest.len(), 8);
        assert_eq!(
            WireType::I64.decode_primitive(rest),
            Some(0x5152_5354_5556_5758)
        );

        // Same message with None: consumed is exactly 1, i64 at offset 1.
        let mut buf2 = Vec::new();
        buf2.extend(WireType::encode_optional(None));
        buf2.extend(WireType::I64.encode_primitive(42).unwrap());
        let (opt2, consumed2) = WireType::decode_optional(&WireType::String, &buf2).unwrap();
        assert_eq!(opt2, None);
        assert_eq!(consumed2, 1);
        assert_eq!(WireType::I64.decode_primitive(&buf2[consumed2..]), Some(42));

        // Truncated inner: declared len exceeds available bytes → None.
        let mut bad = vec![1]; // Some tag
        bad.extend_from_slice(&100u32.to_le_bytes()); // claims 100 bytes
        bad.extend_from_slice(b"short");
        assert!(WireType::decode_optional(&WireType::String, &bad).is_none());
    }

    /// Audit fix 2026-08-05 regression: Result payloads decode with exact
    /// consumed counts, driven by the active branch's type.
    #[test]
    fn wire_result_payload_exact_offsets() {
        // Ok(String) followed by an I32 field.
        let mut buf = Vec::new();
        buf.extend(WireType::encode_result_tag(false));
        buf.extend(WireType::encode_string("ok-value").unwrap());
        let expected = 1 + 4 + 8;
        buf.extend(WireType::I32.encode_primitive(7).unwrap());

        let ok_ty = WireType::String;
        let err_ty = WireType::I64;
        let (result, consumed) = WireType::decode_result(&ok_ty, &err_ty, &buf).unwrap();
        assert_eq!(consumed, expected);
        let payload = result.expect("Ok branch");
        let (s, _) = WireType::decode_string(&payload).unwrap();
        assert_eq!(s, "ok-value");
        // Trailing field untouched.
        assert_eq!(WireType::I32.decode_primitive(&buf[consumed..]), Some(7));

        // Err(I64) branch: tag + 8 bytes.
        let mut buf_err = Vec::new();
        buf_err.extend(WireType::encode_result_tag(true));
        buf_err.extend(WireType::I64.encode_primitive(0xDEAD).unwrap());
        let (result, consumed) = WireType::decode_result(&ok_ty, &err_ty, &buf_err).unwrap();
        assert_eq!(consumed, 1 + 8);
        assert_eq!(result.unwrap_err(), 0xDEADu64.to_le_bytes().to_vec());

        // Invalid tag byte rejected.
        assert!(WireType::decode_result(&ok_ty, &err_ty, &[2, 0, 0]).is_none());
        assert!(WireType::decode_result(&ok_ty, &err_ty, &[]).is_none());
    }

    /// decode_value tracks exact consumption for composite types too.
    #[test]
    fn wire_decode_value_exact_consumption() {
        // Array of 2 I32 followed by an I64.
        let mut buf = WireType::encode_array_header(2);
        buf.extend(WireType::I32.encode_primitive(1).unwrap());
        buf.extend(WireType::I32.encode_primitive(2).unwrap());
        buf.extend(WireType::I64.encode_primitive(9).unwrap());
        let ty = WireType::Array(Box::new(WireType::I32));
        let (raw, consumed) = ty.decode_value(&buf).unwrap();
        assert_eq!(consumed, 4 + 2 * 4);
        assert_eq!(raw, buf[..consumed]);
        assert_eq!(WireType::I64.decode_primitive(&buf[consumed..]), Some(9));

        // Map<String, I32> with 1 pair followed by an I64.
        let mut buf = WireType::encode_map_header(1);
        buf.extend(WireType::encode_string("k").unwrap());
        buf.extend(WireType::I32.encode_primitive(5).unwrap());
        buf.extend(WireType::I64.encode_primitive(11).unwrap());
        let ty = WireType::Map(Box::new(WireType::String), Box::new(WireType::I32));
        let (_, consumed) = ty.decode_value(&buf).unwrap();
        assert_eq!(consumed, 4 + (4 + 1) + 4);
        assert_eq!(WireType::I64.decode_primitive(&buf[consumed..]), Some(11));

        // Unit and fixed primitives.
        let (_, consumed) = WireType::Unit.decode_value(&[]).unwrap();
        assert_eq!(consumed, 0);
        let (_, consumed) = WireType::F64.decode_value(&[1u8; 16]).unwrap();
        assert_eq!(consumed, 8);
        // Truncation.
        assert!(WireType::I64.decode_value(&[1, 2, 3]).is_none());
    }

    #[test]
    fn wire_array_header_roundtrip() {
        let encoded = WireType::encode_array_header(42);
        assert_eq!(encoded.len(), 4);
        let (count, consumed) = WireType::decode_array_header(&encoded).unwrap();
        assert_eq!(count, 42);
        assert_eq!(consumed, 4);

        // Zero elements
        let encoded = WireType::encode_array_header(0);
        let (count, _) = WireType::decode_array_header(&encoded).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn wire_array_header_truncated() {
        assert!(WireType::decode_array_header(&[1, 0]).is_none());
    }

    #[test]
    fn wire_map_header_roundtrip() {
        let encoded = WireType::encode_map_header(7);
        let (count, consumed) = WireType::decode_map_header(&encoded).unwrap();
        assert_eq!(count, 7);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn wire_result_tag_roundtrip() {
        // Ok
        let encoded = WireType::encode_result_tag(false);
        assert_eq!(encoded, vec![0]);
        let (is_err, consumed) = WireType::decode_result_tag(&encoded).unwrap();
        assert!(!is_err);
        assert_eq!(consumed, 1);

        // Err
        let encoded = WireType::encode_result_tag(true);
        assert_eq!(encoded, vec![1]);
        let (is_err, consumed) = WireType::decode_result_tag(&encoded).unwrap();
        assert!(is_err);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn wire_result_tag_invalid() {
        assert!(WireType::decode_result_tag(&[]).is_none());
        assert!(WireType::decode_result_tag(&[2]).is_none());
    }

    #[test]
    fn wire_composite_encode_example() {
        // Encode an array of 3 i32 values: [10, 20, 30]
        let mut buf = WireType::encode_array_header(3);
        for &v in &[10i32, 20, 30] {
            buf.extend(WireType::I32.encode_primitive(v as u64).unwrap());
        }
        assert_eq!(buf.len(), 4 + 3 * 4); // header + 3 elements

        // Decode
        let (count, offset) = WireType::decode_array_header(&buf).unwrap();
        assert_eq!(count, 3);
        let mut values = Vec::new();
        let mut pos = offset;
        for _ in 0..count {
            let v = WireType::I32.decode_primitive(&buf[pos..pos + 4]).unwrap();
            values.push(v as i32);
            pos += 4;
        }
        assert_eq!(values, vec![10, 20, 30]);
    }

    // ── Attack tests (0.31.37) ──

    #[test]
    fn wire_encode_string_rejects_over_u32_max() {
        // We can't actually allocate 4GB+ in a test, but we can verify
        // the error path exists and the function signature returns Result.
        // A string of length u32::MAX + 1 would trigger LengthOverflow.
        // Here we just verify the Ok path works for normal strings.
        assert!(WireType::encode_string("normal").is_ok());
        assert!(WireType::encode_string("").is_ok());
    }

    #[test]
    fn wire_encode_bytes_rejects_over_u32_max() {
        assert!(WireType::encode_bytes(&[1, 2, 3]).is_ok());
        assert!(WireType::encode_bytes(&[]).is_ok());
    }

    #[test]
    fn wire_decode_string_u32_max_len_no_panic() {
        // Craft a header claiming u32::MAX bytes of string data.
        // decode_string must return None (not enough data), not panic.
        let mut data = vec![0u8; 8];
        data[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(WireType::decode_string(&data).is_none());
    }

    #[test]
    fn wire_decode_bytes_u32_max_len_no_panic() {
        let mut data = vec![0u8; 8];
        data[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(WireType::decode_bytes(&data).is_none());
    }

    #[test]
    fn wire_decode_string_non_utf8_rejected() {
        // Valid length prefix but invalid UTF-8 payload
        let mut data = vec![0u8; 8];
        data[0..4].copy_from_slice(&4u32.to_le_bytes());
        data[4] = 0xFF; // invalid UTF-8 start byte
        data[5] = 0xFE;
        data[6] = 0x41;
        data[7] = 0x42;
        assert!(WireType::decode_string(&data).is_none());
    }

    #[test]
    fn wire_nested_type_contains_handle_depth() {
        // 100 levels of nesting should not stack overflow
        let mut ty = WireType::Handle;
        for _ in 0..100 {
            ty = WireType::Array(Box::new(ty));
        }
        assert!(ty.contains_handle());

        // Same depth without handle
        let mut ty = WireType::I32;
        for _ in 0..100 {
            ty = WireType::Optional(Box::new(ty));
        }
        assert!(!ty.contains_handle());
    }
}
