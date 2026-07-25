//! Wire Schema: cross-component serialization format.
//!
//! 0.31.34 (COMPONENT-WIRE-001): Wire never serializes native pointers.
//! All data is serialized as value types (integers, floats, bytes, strings,
//! arrays, maps). Handles are serialized as opaque 64-bit identifiers.
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireEnvelope {
    /// Magic bytes: "MIMI" (0x4D494D49).
    pub magic: u32,
    /// Schema version (semver major.minor packed as u32).
    pub version: u32,
    /// Payload length in bytes.
    pub payload_len: u64,
    /// Payload (serialized message).
    #[serde(skip)]
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
    pub fn from_bytes(data: &[u8]) -> Result<Self, WireError> {
        if data.len() < 16 {
            return Err(WireError::TooShort(data.len()));
        }
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let payload_len =
            u64::from_le_bytes([
                data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
            ]);
        if data.len() < 16 + payload_len as usize {
            return Err(WireError::Truncated {
                expected: 16 + payload_len as usize,
                actual: data.len(),
            });
        }
        let payload = data[16..16 + payload_len as usize].to_vec();
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
    pub fn contains_handle(&self) -> bool {
        match self {
            WireType::Handle => true,
            WireType::Array(inner) => inner.contains_handle(),
            WireType::Map(k, v) => k.contains_handle() || v.contains_handle(),
            WireType::Optional(inner) => inner.contains_handle(),
            WireType::Result(ok, err) => ok.contains_handle() || err.contains_handle(),
            _ => false,
        }
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

/// Wire error.
#[derive(Debug, Clone, PartialEq)]
pub enum WireError {
    /// Bad magic bytes.
    BadMagic(u32),
    /// Version mismatch (major version differs).
    VersionMismatch {
        expected_major: u32,
        got_major: u32,
    },
    /// Payload length mismatch.
    LengthMismatch { declared: u64, actual: u64 },
    /// Data too short for header.
    TooShort(usize),
    /// Data truncated.
    Truncated { expected: usize, actual: usize },
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
}
