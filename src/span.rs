use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Compact identity of a source file within one compilation session.
///
/// The loader owns allocation of non-zero IDs. Parser-only and legacy callers
/// use `UNKNOWN` until they are attached to a loaded source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SourceId(u32);

impl SourceId {
    pub const UNKNOWN: Self = Self(0);

    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn is_known(self) -> bool {
        self.0 != 0
    }
}

/// Stable, public identity of a source independently of allocation order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceKey(String);

impl SourceKey {
    pub fn new(value: impl Into<String>) -> Result<Self, SourceRegistryError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SourceRegistryError::EmptyKey);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build a privacy-preserving identity for a source outside the workspace.
    pub fn external_uri(uri: &str) -> Self {
        Self(format!("external:{:016x}", stable_hash(uri.as_bytes())))
    }

    /// Build a stable, privacy-preserving identity for source text that has no
    /// disk path or canonical URI.  The namespace identifies the API surface,
    /// the label distinguishes logical inputs within that API, and the content
    /// hash prevents two revisions from being treated as the same source.
    pub fn memory(namespace: &str, label: &str, text: &str) -> Result<Self, SourceRegistryError> {
        if namespace.trim().is_empty() || label.trim().is_empty() {
            return Err(SourceRegistryError::EmptyKey);
        }
        Ok(Self(format!(
            "memory:{:016x}:{:016x}:{:016x}",
            stable_hash(namespace.as_bytes()),
            stable_hash(label.as_bytes()),
            stable_hash(text.as_bytes())
        )))
    }
}

impl fmt::Display for SourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTextOrigin {
    Disk,
    Memory,
    Builtin,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub id: SourceId,
    pub key: SourceKey,
    pub canonical_uri: Option<String>,
    pub disk_path: Option<PathBuf>,
    pub text_origin: SourceTextOrigin,
}

impl SourceRecord {
    pub fn new(key: SourceKey, text_origin: SourceTextOrigin) -> Self {
        Self {
            id: SourceId::UNKNOWN,
            key,
            canonical_uri: None,
            disk_path: None,
            text_origin,
        }
    }

    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.canonical_uri = Some(uri.into());
        self
    }

    pub fn with_disk_path(mut self, path: PathBuf) -> Self {
        self.disk_path = Some(path);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRegistryError {
    EmptyKey,
    Exhausted,
    ConflictingRecord(SourceKey),
    ConflictingPath {
        path: PathBuf,
        existing: SourceKey,
        incoming: SourceKey,
    },
    ConflictingUri {
        uri: String,
        existing: SourceKey,
        incoming: SourceKey,
    },
    UnmappedSource(SourceId),
}

impl fmt::Display for SourceRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyKey => f.write_str("source key must not be empty"),
            Self::Exhausted => f.write_str("source registry exhausted u32 SourceId space"),
            Self::ConflictingRecord(key) => {
                write!(f, "conflicting source record for key '{key}'")
            }
            Self::ConflictingPath {
                path,
                existing,
                incoming,
            } => write!(
                f,
                "source path '{}' is registered under both '{}' and '{}'",
                path.display(),
                existing,
                incoming
            ),
            Self::ConflictingUri {
                uri,
                existing,
                incoming,
            } => write!(
                f,
                "source URI '{uri}' is registered under both '{existing}' and '{incoming}'"
            ),
            Self::UnmappedSource(id) => write!(
                f,
                "source id {} is not present in the registry being merged",
                id.raw()
            ),
        }
    }
}

impl std::error::Error for SourceRegistryError {}

/// Compilation-session source interner. `SourceId` is intentionally allocated
/// densely and is never used as a persistent/canonical identity.
#[derive(Debug, Clone, Default)]
pub struct SourceRegistry {
    records: Vec<SourceRecord>,
    by_key: HashMap<SourceKey, SourceId>,
}

/// One fully registered source and the compilation-session registry that owns
/// its dense [`SourceId`].  Parser entry points consume this pair together so
/// a known ID can never be detached from the registry needed to interpret it.
#[derive(Debug, Clone)]
pub struct SourceContext {
    source_id: SourceId,
    registry: SourceRegistry,
}

impl SourceContext {
    pub fn registered(
        source_id: SourceId,
        registry: SourceRegistry,
    ) -> Result<Self, SourceRegistryError> {
        if !source_id.is_known() || registry.record(source_id).is_none() {
            return Err(SourceRegistryError::UnmappedSource(source_id));
        }
        Ok(Self {
            source_id,
            registry,
        })
    }

    /// Register anonymous in-memory text under an explicit logical identity.
    pub fn memory(namespace: &str, label: &str, text: &str) -> Result<Self, SourceRegistryError> {
        let key = SourceKey::memory(namespace, label, text)?;
        let uri = format!("memory://{}", key.as_str().trim_start_matches("memory:"));
        let mut registry = SourceRegistry::default();
        let source_id =
            registry.register(SourceRecord::new(key, SourceTextOrigin::Memory).with_uri(uri))?;
        Self::registered(source_id, registry)
    }

    pub fn source_id(&self) -> SourceId {
        self.source_id
    }

    pub fn registry(&self) -> &SourceRegistry {
        &self.registry
    }

    pub fn into_parts(self) -> (SourceId, SourceRegistry) {
        (self.source_id, self.registry)
    }
}

/// Session-local translation produced when one source registry is interned
/// into another. Canonical identity is always matched by [`SourceKey`]; raw
/// numeric IDs are never copied across registry boundaries.
#[derive(Debug, Clone, Default)]
pub struct SourceIdRemap {
    ids: HashMap<SourceId, SourceId>,
    /// Optional source to attach to spans whose line/column is known but whose
    /// parser session did not yet own a registry. This is only valid for a
    /// single-source, empty-registry AST.
    unknown_target: Option<SourceId>,
}

impl SourceIdRemap {
    pub fn remap(&self, source_id: SourceId) -> Result<SourceId, SourceRegistryError> {
        if !source_id.is_known() {
            return Ok(self.unknown_target.unwrap_or(SourceId::UNKNOWN));
        }
        self.ids
            .get(&source_id)
            .copied()
            .ok_or(SourceRegistryError::UnmappedSource(source_id))
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.unknown_target.is_none()
    }

    /// Build the one deliberate UNKNOWN-source attachment used when a parsed
    /// in-memory AST predates its single source-registry entry.
    pub(crate) fn attach_unknown_to(source_id: SourceId) -> Self {
        debug_assert!(source_id.is_known());
        Self {
            ids: HashMap::new(),
            unknown_target: Some(source_id),
        }
    }

    #[cfg(test)]
    pub(crate) fn erase_for_semantic_comparison(registry: &SourceRegistry) -> Self {
        Self {
            ids: registry
                .records()
                .iter()
                .map(|record| (record.id, SourceId::UNKNOWN))
                .collect(),
            unknown_target: None,
        }
    }
}

impl SourceRegistry {
    pub fn register(&mut self, mut record: SourceRecord) -> Result<SourceId, SourceRegistryError> {
        if let Some(path) = record.disk_path.as_deref() {
            if let Some(existing) = self.records.iter().find(|existing| {
                existing.key != record.key
                    && existing
                        .disk_path
                        .as_deref()
                        .is_some_and(|other| paths_equivalent(other, path))
            }) {
                return Err(SourceRegistryError::ConflictingPath {
                    path: path.to_path_buf(),
                    existing: existing.key.clone(),
                    incoming: record.key,
                });
            }
        }
        if let Some(uri) = record.canonical_uri.as_deref() {
            if let Some(existing) = self.records.iter().find(|existing| {
                existing.key != record.key && existing.canonical_uri.as_deref() == Some(uri)
            }) {
                return Err(SourceRegistryError::ConflictingUri {
                    uri: uri.to_string(),
                    existing: existing.key.clone(),
                    incoming: record.key,
                });
            }
        }

        if let Some(id) = self.by_key.get(&record.key).copied() {
            let index = usize::try_from(id.raw())
                .ok()
                .and_then(|raw| raw.checked_sub(1))
                .expect("registered SourceId must have a non-zero usize index");
            let existing = self
                .records
                .get_mut(index)
                .expect("registered SourceId must resolve to its record");
            let same_disk_source = match (&existing.disk_path, &record.disk_path) {
                (Some(left), Some(right)) => paths_equivalent(left, right),
                _ => false,
            };
            let uri_conflicts = existing.canonical_uri.is_some()
                && record.canonical_uri.is_some()
                && existing.canonical_uri != record.canonical_uri
                && !same_disk_source;
            let path_conflicts =
                existing.disk_path.is_some() && record.disk_path.is_some() && !same_disk_source;
            let origins_compatible = existing.text_origin == record.text_origin
                || matches!(
                    (existing.text_origin, record.text_origin),
                    (SourceTextOrigin::Disk, SourceTextOrigin::Memory)
                        | (SourceTextOrigin::Memory, SourceTextOrigin::Disk)
                );
            if uri_conflicts || path_conflicts || !origins_compatible {
                return Err(SourceRegistryError::ConflictingRecord(record.key));
            }
            if existing.canonical_uri.is_none() {
                existing.canonical_uri = record.canonical_uri;
            }
            if existing.disk_path.is_none() {
                existing.disk_path = record.disk_path;
            }
            if record.text_origin == SourceTextOrigin::Memory {
                existing.text_origin = SourceTextOrigin::Memory;
            }
            return Ok(id);
        }

        let raw =
            u32::try_from(self.records.len() + 1).map_err(|_| SourceRegistryError::Exhausted)?;
        let id = SourceId::new(raw);
        record.id = id;
        self.by_key.insert(record.key.clone(), id);
        self.records.push(record);
        Ok(id)
    }

    pub fn register_key(
        &mut self,
        key: impl Into<String>,
        text_origin: SourceTextOrigin,
    ) -> Result<SourceId, SourceRegistryError> {
        self.register(SourceRecord::new(SourceKey::new(key)?, text_origin))
    }

    pub fn id_for_key(&self, key: &SourceKey) -> Option<SourceId> {
        self.by_key.get(key).copied()
    }

    pub fn id_for_disk_path(&self, path: &Path) -> Option<SourceId> {
        self.records.iter().find_map(|record| {
            record
                .disk_path
                .as_deref()
                .filter(|candidate| paths_equivalent(candidate, path))
                .map(|_| record.id)
        })
    }

    pub fn id_for_uri(&self, uri: &str) -> Option<SourceId> {
        self.records
            .iter()
            .find(|record| record.canonical_uri.as_deref() == Some(uri))
            .map(|record| record.id)
    }

    pub fn record(&self, id: SourceId) -> Option<&SourceRecord> {
        let index = usize::try_from(id.raw()).ok()?.checked_sub(1)?;
        self.records.get(index)
    }

    pub fn key(&self, id: SourceId) -> Option<&SourceKey> {
        self.record(id).map(|record| &record.key)
    }

    pub fn records(&self) -> &[SourceRecord] {
        &self.records
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Intern every record from `other` and return the complete numeric-ID
    /// translation. This is the only supported way to move source-aware AST
    /// nodes between compilation sessions.
    pub fn merge_from(
        &mut self,
        other: &SourceRegistry,
    ) -> Result<SourceIdRemap, SourceRegistryError> {
        let mut merged = self.clone();
        let mut ids = HashMap::with_capacity(other.records.len());
        for record in &other.records {
            let old_id = record.id;
            if !old_id.is_known() {
                return Err(SourceRegistryError::UnmappedSource(old_id));
            }
            let new_id = merged.register(record.clone())?;
            ids.insert(old_id, new_id);
        }
        *self = merged;
        Ok(SourceIdRemap {
            ids,
            unknown_target: None,
        })
    }
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanSourceMismatch {
    pub left: SourceId,
    pub right: SourceId,
}

impl fmt::Display for SpanSourceMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot compose spans from different sources ({} and {})",
            self.left.raw(),
            self.right.raw()
        )
    }
}

impl std::error::Error for SpanSourceMismatch {}

/// A source code span representing a range of text in the source file.
/// Uses compact representation: start position (line, col) + end position (line, col).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub source_id: SourceId,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

impl Span {
    pub const UNKNOWN: Self = Self {
        source_id: SourceId::UNKNOWN,
        start_line: 0,
        start_col: 0,
        end_line: 0,
        end_col: 0,
    };

    /// Create a new span with explicit start and end positions.
    pub fn new(start_line: usize, start_col: usize, end_line: usize, end_col: usize) -> Self {
        Self {
            source_id: SourceId::UNKNOWN,
            start_line,
            start_col,
            end_line,
            end_col,
        }
    }

    /// Create a single-point span (start == end).
    pub fn single(line: usize, col: usize) -> Self {
        Self {
            source_id: SourceId::UNKNOWN,
            start_line: line,
            start_col: col,
            end_line: line,
            end_col: col,
        }
    }

    /// Attach this span to a source registered by the loader.
    pub const fn with_source(mut self, source_id: SourceId) -> Self {
        self.source_id = source_id;
        self
    }

    /// Create a span from a single point to another point.
    pub fn to(&self, other: &Span) -> Result<Self, SpanSourceMismatch> {
        Ok(Self {
            source_id: merged_source(self.source_id, other.source_id)?,
            start_line: self.start_line,
            start_col: self.start_col,
            end_line: other.end_line,
            end_col: other.end_col,
        })
    }

    /// Create a span that covers from self's start to other's end.
    pub fn until(&self, other: &Span) -> Result<Self, SpanSourceMismatch> {
        Ok(Self {
            source_id: merged_source(self.source_id, other.source_id)?,
            start_line: self.start_line,
            start_col: self.start_col,
            end_line: other.end_line,
            end_col: other.end_col,
        })
    }

    /// Check if this span contains a given position.
    pub fn contains(&self, line: usize, col: usize) -> bool {
        if line < self.start_line || line > self.end_line {
            return false;
        }
        if line == self.start_line && col < self.start_col {
            return false;
        }
        if line == self.end_line && col > self.end_col {
            return false;
        }
        true
    }

    /// Get the width of the span in characters.
    /// For multi-line spans, returns the total width across all lines.
    pub fn width(&self) -> usize {
        if self.start_line == self.end_line {
            self.end_col.saturating_sub(self.start_col)
        } else {
            // Multi-line: approximate total width including newline characters.
            // Uses a reasonable default line width of 120 characters (most
            // modern terminals). This is an approximation — the actual source
            // line width is not available here. The impact is only on
            // diagnostics display (wrapping/truncation) and is not
            // semantically significant.
            const LINE_WIDTH: usize = 120;
            let lines = self.end_line.saturating_sub(self.start_line);
            // First line width + intervening lines + last line + newlines
            let first_line = LINE_WIDTH.saturating_sub(self.start_col);
            let mid_lines = lines.saturating_sub(1).saturating_mul(LINE_WIDTH);
            first_line
                .saturating_add(mid_lines)
                .saturating_add(self.end_col)
                .saturating_add(lines)
        }
    }
}

fn merged_source(left: SourceId, right: SourceId) -> Result<SourceId, SpanSourceMismatch> {
    match (left.is_known(), right.is_known()) {
        (true, true) if left != right => Err(SpanSourceMismatch { left, right }),
        (true, _) => Ok(left),
        (_, true) => Ok(right),
        (false, false) => Ok(SourceId::UNKNOWN),
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start_line == self.end_line {
            if self.start_col == self.end_col {
                write!(f, "{}:{}", self.start_line, self.start_col)
            } else {
                write!(f, "{}:{}-{}", self.start_line, self.start_col, self.end_col)
            }
        } else {
            write!(
                f,
                "{}:{}-{}:{}",
                self.start_line, self.start_col, self.end_line, self.end_col
            )
        }
    }
}

/// Convert from (line, col) token positions to a Span.
/// Assumes line/col are 1-indexed (as in the lexer).
impl From<(usize, usize)> for Span {
    fn from((line, col): (usize, usize)) -> Self {
        Self::single(line, col)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SourceContext, SourceId, SourceKey, SourceRecord, SourceRegistry, SourceTextOrigin, Span,
    };

    #[test]
    fn source_identity_survives_span_composition() {
        let source = SourceId::new(7);
        let start = Span::single(2, 3).with_source(source);
        let end = Span::single(4, 5).with_source(source);

        assert_eq!(start.to(&end).expect("same source").source_id, source);
        assert_eq!(start.until(&end).expect("same source").source_id, source);
        assert!(source.is_known());
        assert_eq!(source.raw(), 7);
    }

    #[test]
    fn parser_created_spans_have_explicit_unknown_source() {
        assert_eq!(Span::single(1, 1).source_id, SourceId::UNKNOWN);
        assert!(!SourceId::UNKNOWN.is_known());
    }

    #[test]
    fn registry_allocates_dense_session_ids_and_interns_keys() {
        let mut registry = SourceRegistry::default();
        let main = registry
            .register_key("workspace:src/main.mimi", SourceTextOrigin::Disk)
            .expect("register main");
        let same = registry
            .register_key("workspace:src/main.mimi", SourceTextOrigin::Disk)
            .expect("intern main");
        let dependency = registry
            .register_key("workspace:src/dependency.mimi", SourceTextOrigin::Disk)
            .expect("register dependency");

        assert_eq!(main, SourceId::new(1));
        assert_eq!(same, main);
        assert_eq!(dependency, SourceId::new(2));
        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry.key(main).map(SourceKey::as_str),
            Some("workspace:src/main.mimi")
        );
    }

    #[test]
    fn registry_rejects_conflicting_records_for_one_key() {
        let mut registry = SourceRegistry::default();
        let key = SourceKey::new("workspace:src/main.mimi").expect("key");
        registry
            .register(
                SourceRecord::new(key.clone(), SourceTextOrigin::Disk)
                    .with_uri("file:///workspace/src/main.mimi"),
            )
            .expect("first record");
        assert!(registry
            .register(
                SourceRecord::new(key, SourceTextOrigin::Disk)
                    .with_uri("file:///other/src/main.mimi"),
            )
            .is_err());
    }

    #[test]
    fn spans_from_distinct_known_sources_cannot_be_composed() {
        let left = Span::single(1, 1).with_source(SourceId::new(1));
        let right = Span::single(1, 2).with_source(SourceId::new(2));
        assert!(left.to(&right).is_err());
        assert!(left.until(&right).is_err());
    }

    #[test]
    fn registry_union_remaps_dense_ids_by_stable_key() {
        let mut target = SourceRegistry::default();
        let target_b = target
            .register_key("workspace:b.mimi", SourceTextOrigin::Disk)
            .expect("target b");

        let mut incoming = SourceRegistry::default();
        let incoming_a = incoming
            .register_key("workspace:a.mimi", SourceTextOrigin::Disk)
            .expect("incoming a");
        let incoming_b = incoming
            .register_key("workspace:b.mimi", SourceTextOrigin::Disk)
            .expect("incoming b");

        assert_eq!(target_b, SourceId::new(1));
        assert_eq!(incoming_a, SourceId::new(1));
        assert_eq!(incoming_b, SourceId::new(2));

        let remap = target.merge_from(&incoming).expect("merge registries");
        let target_a = target
            .id_for_key(&SourceKey::new("workspace:a.mimi").expect("key"))
            .expect("merged a");
        assert_eq!(target_a, SourceId::new(2));
        assert_eq!(remap.remap(incoming_a).expect("remap a"), target_a);
        assert_eq!(remap.remap(incoming_b).expect("remap b"), target_b);
        assert_eq!(
            remap.remap(SourceId::UNKNOWN).expect("unknown"),
            SourceId::UNKNOWN
        );
    }

    #[test]
    fn registry_union_enriches_same_source_and_prefers_memory_origin() {
        let path = std::env::temp_dir().join("mimi_registry_union_source.mimi");
        let key = SourceKey::new("workspace:main.mimi").expect("key");
        let mut target = SourceRegistry::default();
        let id = target
            .register(SourceRecord::new(key.clone(), SourceTextOrigin::Disk))
            .expect("disk record");
        let mut incoming = SourceRegistry::default();
        incoming
            .register(
                SourceRecord::new(key, SourceTextOrigin::Memory)
                    .with_uri("file:///tmp/mimi_registry_union_source.mimi")
                    .with_disk_path(path.clone()),
            )
            .expect("memory record");

        let remap = target.merge_from(&incoming).expect("compatible merge");
        assert_eq!(remap.remap(SourceId::new(1)).expect("remap"), id);
        let record = target.record(id).expect("record");
        assert_eq!(record.text_origin, SourceTextOrigin::Memory);
        assert_eq!(record.disk_path.as_deref(), Some(path.as_path()));
        assert_eq!(
            record.canonical_uri.as_deref(),
            Some("file:///tmp/mimi_registry_union_source.mimi")
        );
    }

    #[test]
    fn registry_rejects_two_keys_for_one_disk_source() {
        let path = std::env::temp_dir().join("mimi_registry_conflicting_key.mimi");
        let mut registry = SourceRegistry::default();
        registry
            .register(
                SourceRecord::new(
                    SourceKey::new("workspace:main.mimi").expect("key"),
                    SourceTextOrigin::Disk,
                )
                .with_disk_path(path.clone()),
            )
            .expect("first identity");
        let error = registry
            .register(
                SourceRecord::new(
                    SourceKey::new("external:1234").expect("key"),
                    SourceTextOrigin::Disk,
                )
                .with_disk_path(path),
            )
            .expect_err("one path cannot have two canonical keys");
        assert!(error.to_string().contains("registered under both"));
    }

    #[test]
    fn failed_registry_union_is_atomic() {
        let shared_path = std::env::temp_dir().join("mimi_registry_atomic_conflict.mimi");
        let mut target = SourceRegistry::default();
        target
            .register(
                SourceRecord::new(
                    SourceKey::new("workspace:existing.mimi").expect("key"),
                    SourceTextOrigin::Disk,
                )
                .with_disk_path(shared_path.clone()),
            )
            .expect("existing source");

        let mut incoming = SourceRegistry::default();
        incoming
            .register_key("workspace:would-be-added.mimi", SourceTextOrigin::Disk)
            .expect("first incoming");
        incoming
            .register(
                SourceRecord::new(
                    SourceKey::new("workspace:conflict.mimi").expect("key"),
                    SourceTextOrigin::Disk,
                )
                .with_disk_path(shared_path),
            )
            .expect("incoming registry is internally valid");

        assert!(target.merge_from(&incoming).is_err());
        assert_eq!(target.len(), 1);
        assert!(target
            .id_for_key(&SourceKey::new("workspace:would-be-added.mimi").expect("key"))
            .is_none());
    }

    #[test]
    fn memory_context_is_stable_and_label_isolated() {
        let source = "func main() -> i32 { 0 }";
        let first = SourceContext::memory("verifier", "contract", source).expect("first");
        let second = SourceContext::memory("verifier", "contract", source).expect("second");
        let other = SourceContext::memory("verifier", "ffi", source).expect("other label");

        assert_eq!(
            first.registry().key(first.source_id()),
            second.registry().key(second.source_id())
        );
        assert_ne!(
            first.registry().key(first.source_id()),
            other.registry().key(other.source_id())
        );
        let key = first
            .registry()
            .key(first.source_id())
            .expect("memory key")
            .as_str();
        assert!(key.starts_with("memory:"));
        assert!(!key.contains(source));
    }
}
