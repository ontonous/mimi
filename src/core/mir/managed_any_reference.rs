//! Independent semantic values for receipted ordinary Map Any snapshots.
//!
//! The reference executor owns Rust payloads, never native owner tokens or
//! legacy ValueHandle bits. Descriptor facts are explicit and checked before
//! payload access; the production table's generation validation remains an
//! independent native implementation of the same contract.

#[derive(Debug, Clone, PartialEq, Eq)]
enum Payload {
    I32(i32),
    String(String),
}

/// An independently owned snapshot with a pinned closed descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirManagedAnyValue {
    kind: u32,
    version: u32,
    payload: Payload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirManagedAnyReferenceError {
    Capacity,
    KindMismatch,
    VersionMismatch,
    DescriptorMismatch,
}

impl MirManagedAnyValue {
    pub(crate) const I32_KIND: u32 = 1;
    pub(crate) const STRING_KIND: u32 = 2;
    pub(crate) const DESCRIPTOR_VERSION: u32 = 1;

    pub(crate) fn from_i32(value: i32) -> Self {
        Self {
            kind: Self::I32_KIND,
            version: Self::DESCRIPTOR_VERSION,
            payload: Payload::I32(value),
        }
    }

    pub(crate) fn from_string(value: &str) -> Result<Self, MirManagedAnyReferenceError> {
        Ok(Self {
            kind: Self::STRING_KIND,
            version: Self::DESCRIPTOR_VERSION,
            payload: Payload::String(copy_string(value)?),
        })
    }

    pub(crate) fn validate_descriptor(&self) -> Result<(), MirManagedAnyReferenceError> {
        if self.version != Self::DESCRIPTOR_VERSION {
            return Err(MirManagedAnyReferenceError::VersionMismatch);
        }
        match (&self.payload, self.kind) {
            (Payload::I32(_), Self::I32_KIND) | (Payload::String(_), Self::STRING_KIND) => Ok(()),
            _ => Err(MirManagedAnyReferenceError::DescriptorMismatch),
        }
    }

    pub(crate) fn read_i32(
        &self,
        expected_kind: u32,
        expected_version: u32,
    ) -> Result<i32, MirManagedAnyReferenceError> {
        self.validate_expected(expected_kind, expected_version)?;
        match &self.payload {
            Payload::I32(value) => Ok(*value),
            Payload::String(_) => Err(MirManagedAnyReferenceError::KindMismatch),
        }
    }

    pub(crate) fn copy_string(
        &self,
        expected_kind: u32,
        expected_version: u32,
    ) -> Result<String, MirManagedAnyReferenceError> {
        self.validate_expected(expected_kind, expected_version)?;
        match &self.payload {
            Payload::String(value) => copy_string(value),
            Payload::I32(_) => Err(MirManagedAnyReferenceError::KindMismatch),
        }
    }

    /// Compare a borrowed Mimi String against this pinned dynamic value.
    /// A valid value with a different payload kind is simply unequal; malformed
    /// tag/version state remains an explicit descriptor error. This avoids
    /// allocating a String snapshot just to perform equality.
    pub(crate) fn equals_string(&self, value: &str) -> Result<bool, MirManagedAnyReferenceError> {
        self.validate_descriptor()?;
        Ok(match &self.payload {
            Payload::I32(_) => false,
            Payload::String(payload) => payload == value,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_descriptor(mut self, kind: u32, version: u32) -> Self {
        self.kind = kind;
        self.version = version;
        self
    }

    fn validate_expected(
        &self,
        expected_kind: u32,
        expected_version: u32,
    ) -> Result<(), MirManagedAnyReferenceError> {
        if expected_version != Self::DESCRIPTOR_VERSION {
            return Err(MirManagedAnyReferenceError::VersionMismatch);
        }
        self.validate_descriptor()?;
        if expected_kind != self.kind {
            return Err(MirManagedAnyReferenceError::KindMismatch);
        }
        Ok(())
    }

    pub(crate) fn try_clone(&self) -> Result<Self, MirManagedAnyReferenceError> {
        self.validate_descriptor()?;
        match &self.payload {
            Payload::I32(value) => Ok(Self::from_i32(*value)),
            Payload::String(value) => Self::from_string(value),
        }
    }
}

fn copy_string(value: &str) -> Result<String, MirManagedAnyReferenceError> {
    let mut result = String::new();
    result
        .try_reserve_exact(value.len())
        .map_err(|_| MirManagedAnyReferenceError::Capacity)?;
    result.push_str(value);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_any_reference_unpacks_exact_kind_and_preserves_source() {
        let value = MirManagedAnyValue::from_string("owned\0🧪值").unwrap();
        assert_eq!(
            value.read_i32(1, 1),
            Err(MirManagedAnyReferenceError::KindMismatch)
        );
        let first = value.copy_string(2, 1).unwrap();
        let second = value.copy_string(2, 1).unwrap();
        assert_eq!(first, "owned\0🧪值");
        assert_eq!(second, first);
        drop(value);
        assert_eq!(second, "owned\0🧪值");
    }

    #[test]
    fn managed_any_reference_clones_are_owned_after_original_drop() {
        let value = MirManagedAnyValue::from_string("").unwrap();
        let cloned = value.try_clone().unwrap();
        drop(value);
        assert_eq!(cloned.copy_string(2, 1).unwrap(), "");
        let zero = MirManagedAnyValue::from_i32(0);
        assert_eq!(zero.read_i32(1, 1).unwrap(), 0);
        assert_eq!(
            zero.copy_string(2, 1),
            Err(MirManagedAnyReferenceError::KindMismatch)
        );
    }

    #[test]
    fn managed_any_reference_rejects_wrong_versions_and_forged_descriptor() {
        let value = MirManagedAnyValue::from_i32(42);
        assert_eq!(
            value.read_i32(1, 2),
            Err(MirManagedAnyReferenceError::VersionMismatch)
        );
        let wrong_version = MirManagedAnyValue {
            version: 2,
            ..value.clone()
        };
        assert_eq!(
            wrong_version.try_clone(),
            Err(MirManagedAnyReferenceError::VersionMismatch)
        );
        let forged_kind = MirManagedAnyValue { kind: 2, ..value };
        assert_eq!(
            forged_kind.read_i32(2, 1),
            Err(MirManagedAnyReferenceError::DescriptorMismatch)
        );
        assert_eq!(
            forged_kind.try_clone(),
            Err(MirManagedAnyReferenceError::DescriptorMismatch)
        );
    }

    #[test]
    fn managed_any_string_equality_rejects_malformed_runtime_descriptors() {
        let string = MirManagedAnyValue::from_string("owned\0🧪值").unwrap();
        assert_eq!(string.equals_string("owned\0🧪值"), Ok(true));
        assert_eq!(string.equals_string("other"), Ok(false));

        let wrong_version = MirManagedAnyValue {
            version: 2,
            ..string.clone()
        };
        assert_eq!(
            wrong_version.equals_string("owned\0🧪值"),
            Err(MirManagedAnyReferenceError::VersionMismatch)
        );
        let forged_kind = MirManagedAnyValue { kind: 1, ..string };
        assert_eq!(
            forged_kind.equals_string("owned\0🧪值"),
            Err(MirManagedAnyReferenceError::DescriptorMismatch)
        );
    }
}
