//! Versioned compact snapshot artifact save/load support.
//!
//! Snapshot files are deployment artifacts for prebuilt local authorization data. They are treated
//! as untrusted input during load: headers, section bounds, symbol ids, row ids, index order, and
//! checksums are validated before a service publishes the loaded snapshot.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use thiserror::Error;

use crate::{
    domain::DomainError,
    error::ZanzibarError,
    model::NamespaceConfig,
    policy,
    relationship::{IndexedRelationshipStore, RelationshipStoreView, StoreError},
    revision::{Revision, SchemaHash},
    schema::{self, CompiledSchema},
};

const MAGIC_PREFIX: [u8; 7] = *b"SZSNAP\0";
const FORMAT_V2_MAGIC: [u8; 8] = *b"SZSNAP\0\x02";
const FORMAT_V3_MAGIC: [u8; 8] = *b"SZSNAP\0\x03";
const CURRENT_FORMAT_VERSION: SnapshotFormatVersion = SnapshotFormatVersion::V3;
const HEADER_LEN: usize = 76;
const HEADER_LEN_U32: u32 = 76;
const DIRECTORY_ENTRY_LEN: usize = 28;
const FOOTER_LEN: usize = 32;
const REQUIRED_SECTION_COUNT: usize = 11;
const REQUIRED_SECTION_COUNT_U32: u32 = 11;
const MAX_SCHEMA_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_ZSTD_LEVEL: i32 = 3;

/// Options used when saving a compact snapshot artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotSaveOptions {
    /// Snapshot compression mode.
    pub compression: SnapshotCompression,
    /// Zstd compression level used when `compression` is [`SnapshotCompression::Zstd`].
    pub zstd_level: i32,
    /// Whether serialized lookup indexes are included.
    ///
    /// Snapshot artifacts require indexes because fast loading depends on stable sorted arrays.
    pub include_indexes: bool,
    /// Index profile encoded in the snapshot artifact.
    pub index_profile: IndexProfile,
}

impl Default for SnapshotSaveOptions {
    fn default() -> Self {
        Self {
            compression: SnapshotCompression::None,
            zstd_level: DEFAULT_ZSTD_LEVEL,
            include_indexes: true,
            index_profile: IndexProfile::Full,
        }
    }
}

impl SnapshotSaveOptions {
    /// Creates save options for an uncompressed `.szsnap` artifact.
    #[must_use]
    pub fn uncompressed() -> Self {
        Self::default()
    }

    /// Creates save options for a zstd-wrapped `.szsnap.zst` artifact.
    #[must_use]
    pub fn zstd() -> Self {
        Self {
            compression: SnapshotCompression::Zstd,
            ..Self::default()
        }
    }

    /// Returns options with a specific zstd compression level.
    #[must_use]
    pub fn with_zstd_level(mut self, zstd_level: i32) -> Self {
        self.zstd_level = zstd_level;
        self
    }

    pub(crate) const fn section_layout(self) -> SnapshotEncodingLayout {
        match self.compression {
            SnapshotCompression::None => SnapshotEncodingLayout::Compact,
            SnapshotCompression::Zstd => SnapshotEncodingLayout::CompressionFriendly,
        }
    }
}

/// Snapshot compression mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotCompression {
    /// Store all sections uncompressed.
    None,
    /// Wrap a valid `.szsnap` payload in a single zstd frame.
    ///
    /// The inner payload may use section widths chosen for better compression while remaining a
    /// normal versioned snapshot parsed by the same reader.
    Zstd,
}

/// Internal section layout used before any outer compression is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotEncodingLayout {
    /// Minimize the raw `.szsnap` artifact size with variable-width section fields.
    Compact,
    /// Preserve fixed-width row and symbol metadata when an outer compressor can absorb zeros.
    CompressionFriendly,
}

/// Supported compact snapshot artifact format versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotFormatVersion {
    /// Version 2 stores fixed-width `u32` posting overflow row ids.
    V2,
    /// Version 3 stores posting overflow row ids as per-range delta varints.
    V3,
}

impl SnapshotFormatVersion {
    const fn raw(self) -> u16 {
        match self {
            Self::V2 => 2,
            Self::V3 => 3,
        }
    }

    const fn magic(self) -> [u8; 8] {
        match self {
            Self::V2 => FORMAT_V2_MAGIC,
            Self::V3 => FORMAT_V3_MAGIC,
        }
    }

    fn from_header(magic: [u8; 8], version: u16) -> Result<Self, SnapshotIoError> {
        let Some(prefix) = magic.get(..MAGIC_PREFIX.len()) else {
            return Err(SnapshotIoError::Format {
                reason: "snapshot magic is invalid",
            });
        };
        if prefix != MAGIC_PREFIX {
            return Err(SnapshotIoError::Format {
                reason: "snapshot magic is invalid",
            });
        }
        match (magic, version) {
            (FORMAT_V2_MAGIC, 2) => Ok(Self::V2),
            (FORMAT_V3_MAGIC, 3) => Ok(Self::V3),
            _ => Err(SnapshotIoError::Format {
                reason: "snapshot format version is unsupported",
            }),
        }
    }
}

/// Relationship index profile encoded in runtime state and snapshot artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexProfile {
    /// All indexes required by the complete public API.
    Full,
    /// Resource-side indexes for check, expand, and object-bounded permission audit.
    CheckOnly,
    /// Resource-side indexes plus object-bounded audit support.
    CheckAndObjectAudit,
}

impl IndexProfile {
    pub(crate) const fn flag_bits(self) -> u16 {
        match self {
            Self::Full => 0,
            Self::CheckOnly => 1,
            Self::CheckAndObjectAudit => 2,
        }
    }

    fn from_flag_bits(value: u16) -> Result<Self, SnapshotIoError> {
        match value {
            0 => Ok(Self::Full),
            1 => Ok(Self::CheckOnly),
            2 => Ok(Self::CheckAndObjectAudit),
            _ => Err(SnapshotIoError::Format {
                reason: "snapshot index profile is unsupported",
            }),
        }
    }

    /// Returns true when subject reverse lookup indexes are present.
    #[must_use]
    pub const fn supports_subject_reverse_lookup(self) -> bool {
        matches!(self, Self::Full)
    }

    pub(crate) const fn satisfies(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Full, _)
                | (
                    Self::CheckAndObjectAudit,
                    Self::CheckAndObjectAudit | Self::CheckOnly
                )
                | (Self::CheckOnly, Self::CheckOnly)
        )
    }

    pub(crate) const fn supports_broad_resource_indexes(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Options used when loading a compact snapshot artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLoadOptions {
    /// Snapshot compression mode expected for the file.
    pub compression: SnapshotCompression,
    /// Runtime index profile to construct from serialized sorted arrays.
    pub profile: SnapshotLoadProfile,
    /// Validation mode to apply while loading the artifact.
    pub validation: SnapshotValidationMode,
    /// Integrity proof expected before publishing the loaded snapshot.
    pub integrity: SnapshotIntegrityMode,
    /// Maximum accepted file size in bytes.
    ///
    /// For zstd snapshots this cap is enforced against both the compressed file and the
    /// decompressed inner snapshot payload. The inner payload is a normal versioned snapshot and
    /// may be larger than an independently saved uncompressed artifact because zstd saves can
    /// choose compression-friendly section widths.
    pub max_file_bytes: NonZeroU64,
    /// Minimum index capability required by the caller.
    pub required_index_profile: IndexProfile,
}

impl Default for SnapshotLoadOptions {
    fn default() -> Self {
        Self {
            compression: SnapshotCompression::None,
            profile: SnapshotLoadProfile::FastLoad,
            validation: SnapshotValidationMode::Full,
            integrity: SnapshotIntegrityMode::Checksum,
            max_file_bytes: non_zero_u64(DEFAULT_MAX_FILE_BYTES),
            required_index_profile: IndexProfile::Full,
        }
    }
}

impl SnapshotLoadOptions {
    /// Creates load options for an uncompressed `.szsnap` artifact.
    #[must_use]
    pub fn uncompressed() -> Self {
        Self::default()
    }

    /// Creates load options for a zstd-wrapped `.szsnap.zst` artifact.
    #[must_use]
    pub fn zstd() -> Self {
        Self {
            compression: SnapshotCompression::Zstd,
            ..Self::default()
        }
    }
}

/// Runtime index profile for a loaded snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotLoadProfile {
    /// Keep sorted-array indexes and binary-search on query.
    FastLoad,
    /// Rebuild hash-backed in-memory indexes after validation.
    Latency,
}

/// Validation boundary to apply when loading a compact snapshot artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotValidationMode {
    /// Treat the file as hostile input and revalidate all semantic row/index invariants.
    Full,
    /// Trust a build-pipeline validated artifact and perform only structural checks at startup.
    TrustedFastLoad,
}

/// Integrity proof mode to apply to the snapshot envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotIntegrityMode {
    /// Verify the file footer checksum during load.
    Checksum,
    /// Assume the artifact bytes were verified by an external content-address or signature layer.
    External,
}

/// Benchmark-only snapshot load phase timing evidence.
#[cfg_attr(not(feature = "bench-internals"), doc(hidden))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotLoadPhaseTimings {
    /// Reading bytes from the filesystem.
    pub file_read: Duration,
    /// Decompressing a zstd payload, if configured.
    pub decompression: Duration,
    /// Parsing header and section directory metadata.
    pub header_and_sections: Duration,
    /// Verifying the snapshot footer checksum.
    pub checksum: Duration,
    /// Parsing and compiling the schema section.
    pub schema_parse_compile: Duration,
    /// Decoding symbol sections.
    pub symbols: Duration,
    /// Decoding relationship rows.
    pub rows: Duration,
    /// Decoding relationship indexes.
    pub indexes: Duration,
    /// Publishing the loaded structures into the returned value.
    pub publish: Duration,
}

/// Errors returned by compact snapshot save/load operations.
#[derive(Debug, Error)]
pub enum SnapshotIoError {
    /// Filesystem I/O failed.
    #[error("snapshot io failed")]
    Io {
        /// Source I/O error.
        #[source]
        source: io::Error,
    },

    /// The snapshot artifact failed format validation.
    #[error("snapshot format error: {reason}")]
    Format {
        /// Static format failure reason.
        reason: &'static str,
    },

    /// A configured or format-defined limit was exceeded.
    #[error("snapshot limit exceeded: {component}")]
    LimitExceeded {
        /// Component that exceeded its limit.
        component: &'static str,
    },

    /// A public option is not supported by the current artifact version.
    #[error("snapshot option is unsupported: {option}")]
    UnsupportedOption {
        /// Unsupported option name.
        option: &'static str,
    },

    /// Schema text in the artifact could not be parsed or compiled.
    #[error("snapshot schema failed")]
    Schema {
        /// Source schema error.
        #[source]
        source: ZanzibarError,
    },

    /// Compact relationship data failed validation.
    #[error("snapshot relationship store failed")]
    Store {
        /// Source store error.
        #[source]
        source: StoreError,
    },

    /// Domain identifier validation failed while checking compact rows.
    #[error("snapshot domain validation failed")]
    Domain {
        /// Source domain error.
        #[source]
        source: DomainError,
    },
}

impl From<io::Error> for SnapshotIoError {
    fn from(source: io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<StoreError> for SnapshotIoError {
    fn from(source: StoreError) -> Self {
        Self::Store { source }
    }
}

impl From<DomainError> for SnapshotIoError {
    fn from(source: DomainError) -> Self {
        Self::Domain { source }
    }
}

pub(crate) struct LoadedSnapshot {
    pub(crate) configs: HashMap<String, NamespaceConfig>,
    pub(crate) schema: CompiledSchema,
    pub(crate) relationships: Arc<RelationshipStoreView>,
    pub(crate) revision: Revision,
    pub(crate) schema_hash: SchemaHash,
}

/// Stable snapshot section identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum SectionKind {
    Schema = 1,
    SymbolBytes = 2,
    SymbolTable = 3,
    RelationshipRows = 4,
    IndexDirectory = 5,
    IndexKeys = 6,
    PostingRanges = 7,
    PostingRowIds = 8,
    SymbolHashes = 9,
    SymbolLookup = 10,
    Footer = 11,
}

impl SectionKind {
    fn from_raw(value: u16) -> Result<Self, SnapshotIoError> {
        match value {
            1 => Ok(Self::Schema),
            2 => Ok(Self::SymbolBytes),
            3 => Ok(Self::SymbolTable),
            4 => Ok(Self::RelationshipRows),
            5 => Ok(Self::IndexDirectory),
            6 => Ok(Self::IndexKeys),
            7 => Ok(Self::PostingRanges),
            8 => Ok(Self::PostingRowIds),
            9 => Ok(Self::SymbolHashes),
            10 => Ok(Self::SymbolLookup),
            11 => Ok(Self::Footer),
            _ => Err(SnapshotIoError::Format {
                reason: "unknown snapshot section kind",
            }),
        }
    }

    const fn raw(self) -> u16 {
        self as u16
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotHeader {
    pub(crate) format_version: SnapshotFormatVersion,
    pub(crate) schema_hash: SchemaHash,
    pub(crate) relationship_count: u32,
    pub(crate) symbol_count: u32,
    pub(crate) created_revision: Revision,
    pub(crate) index_profile: IndexProfile,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotSection<'a> {
    bytes: &'a [u8],
    flags: u16,
    row_count: u64,
}

impl<'a> SnapshotSection<'a> {
    /// Returns the raw section bytes.
    #[must_use]
    pub(crate) const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Returns the section-local encoding flags from the directory entry.
    #[must_use]
    pub(crate) const fn flags(self) -> u16 {
        self.flags
    }

    /// Returns the section row count from the directory entry.
    #[must_use]
    pub(crate) const fn row_count(self) -> u64 {
        self.row_count
    }
}

#[derive(Debug)]
pub(crate) struct SnapshotReader<'a> {
    header: SnapshotHeader,
    sections: SnapshotSections<'a>,
}

type SnapshotSections<'a> = [Option<SnapshotSection<'a>>; REQUIRED_SECTION_COUNT];

impl<'a> SnapshotReader<'a> {
    /// Parses and validates the outer snapshot envelope.
    pub(crate) fn parse(
        bytes: &'a [u8],
        integrity: SnapshotIntegrityMode,
        timings: Option<&mut SnapshotLoadPhaseTimings>,
    ) -> Result<Self, SnapshotIoError> {
        let header = parse_header(bytes)?;
        let directory_start = checked_usize_from_u32(HEADER_LEN_U32)?;
        let section_count = parse_section_count(bytes)?;
        if section_count != REQUIRED_SECTION_COUNT {
            return Err(SnapshotIoError::Format {
                reason: "unexpected snapshot section count",
            });
        }
        let directory_len = checked_mul_usize(section_count, DIRECTORY_ENTRY_LEN)?;
        let directory_end = checked_add_usize(directory_start, directory_len)?;
        let directory_bytes =
            bytes
                .get(directory_start..directory_end)
                .ok_or(SnapshotIoError::Format {
                    reason: "section directory is out of bounds",
                })?;

        let mut cursor = BinaryCursor::new(directory_bytes);
        let mut entries = Vec::with_capacity(section_count);
        let mut sections = empty_sections();
        for _ in 0..section_count {
            let raw_kind = cursor.read_u16()?;
            let kind = SectionKind::from_raw(raw_kind)?;
            let flags = cursor.read_u16()?;
            validate_section_flags(header.format_version, kind, flags)?;
            let offset = cursor.read_u64()?;
            let len = cursor.read_u64()?;
            let row_count = cursor.read_u64()?;
            let slot = section_slot(kind);
            if sections.get(slot).copied().flatten().is_some() {
                return Err(SnapshotIoError::Format {
                    reason: "duplicate snapshot section",
                });
            }
            let start = checked_usize_from_u64(offset)?;
            let length = checked_usize_from_u64(len)?;
            let end = checked_add_usize(start, length)?;
            if start < directory_end || end > bytes.len() {
                return Err(SnapshotIoError::Format {
                    reason: "snapshot section is out of bounds",
                });
            }
            let section_bytes = bytes.get(start..end).ok_or(SnapshotIoError::Format {
                reason: "snapshot section range is invalid",
            })?;
            if let Some(section) = sections.get_mut(slot) {
                *section = Some(SnapshotSection {
                    bytes: section_bytes,
                    flags,
                    row_count,
                });
            }
            entries.push((offset, len, kind));
        }

        validate_required_sections(&sections)?;
        validate_non_overlapping_sections(&mut entries)?;
        let checksum_start = Instant::now();
        validate_footer(bytes, &sections, integrity)?;
        if let Some(timings) = timings {
            timings.checksum = checksum_start.elapsed();
        }
        Ok(Self { header, sections })
    }

    /// Returns the parsed snapshot header.
    #[must_use]
    pub(crate) const fn header(&self) -> SnapshotHeader {
        self.header
    }

    /// Returns a required section.
    pub(crate) fn section(
        &self,
        kind: SectionKind,
    ) -> Result<SnapshotSection<'a>, SnapshotIoError> {
        self.sections
            .get(section_slot(kind))
            .copied()
            .flatten()
            .ok_or(SnapshotIoError::Format {
                reason: "missing required snapshot section",
            })
    }
}

const fn empty_sections<'a>() -> SnapshotSections<'a> {
    [None; REQUIRED_SECTION_COUNT]
}

const fn section_slot(kind: SectionKind) -> usize {
    kind as usize - 1
}

#[derive(Debug, Default)]
pub(crate) struct SnapshotSectionWriter {
    sections: Vec<SectionPayload>,
}

impl SnapshotSectionWriter {
    /// Adds one snapshot section payload.
    pub(crate) fn add_section(
        &mut self,
        kind: SectionKind,
        bytes: Vec<u8>,
        row_count: u64,
    ) -> Result<(), SnapshotIoError> {
        self.add_section_with_flags(kind, 0, bytes, row_count)
    }

    /// Adds one snapshot section payload with section-local encoding flags.
    pub(crate) fn add_section_with_flags(
        &mut self,
        kind: SectionKind,
        flags: u16,
        bytes: Vec<u8>,
        row_count: u64,
    ) -> Result<(), SnapshotIoError> {
        if self.sections.iter().any(|section| section.kind == kind) {
            return Err(SnapshotIoError::Format {
                reason: "duplicate snapshot section",
            });
        }
        self.sections.push(SectionPayload {
            kind,
            flags,
            bytes,
            row_count,
        });
        Ok(())
    }

    fn row_count(&self, kind: SectionKind) -> Result<u64, SnapshotIoError> {
        self.sections
            .iter()
            .find(|section| section.kind == kind)
            .map(|section| section.row_count)
            .ok_or(SnapshotIoError::Format {
                reason: "missing required snapshot section",
            })
    }

    fn into_sorted_sections(mut self) -> Vec<SectionPayload> {
        self.sections.sort_by_key(|section| section.kind);
        self.sections
    }
}

#[derive(Debug)]
struct SectionPayload {
    kind: SectionKind,
    flags: u16,
    bytes: Vec<u8>,
    row_count: u64,
}

#[derive(Debug)]
struct SectionDirectoryEntry {
    kind: SectionKind,
    flags: u16,
    offset: u64,
    len: u64,
    row_count: u64,
}

pub(crate) fn save_snapshot_file(
    path: &Path,
    snapshot: &crate::revision::PublishedSnapshot,
    options: SnapshotSaveOptions,
) -> Result<(), SnapshotIoError> {
    if !options.include_indexes {
        return Err(SnapshotIoError::UnsupportedOption {
            option: "include_indexes=false",
        });
    }
    validate_compression_options(options)?;

    let mut writer = SnapshotSectionWriter::default();
    let schema_source = policy::canonical_schema_source(snapshot.configs());
    if schema_source.len() > MAX_SCHEMA_BYTES {
        return Err(SnapshotIoError::LimitExceeded {
            component: "schema section",
        });
    }
    writer.add_section(SectionKind::Schema, schema_source.into_bytes(), 1)?;
    snapshot.relationships().encode_snapshot_sections(
        &mut writer,
        options.index_profile,
        options.section_layout(),
    )?;
    match options.compression {
        SnapshotCompression::None => {
            write_uncompressed_snapshot_file(path, snapshot, writer, options.index_profile)
        }
        SnapshotCompression::Zstd => {
            let bytes = encode_file(snapshot, writer, options.index_profile)?;
            fs::write(path, encode_payload(bytes, options)?)?;
            Ok(())
        }
    }
}

pub(crate) fn load_snapshot_file(
    path: &Path,
    options: SnapshotLoadOptions,
) -> Result<LoadedSnapshot, SnapshotIoError> {
    load_snapshot_file_inner(path, options, None)
}

#[cfg(feature = "bench-internals")]
/// Loads a snapshot and returns benchmark-only phase timing evidence.
///
/// # Errors
///
/// Returns [`SnapshotIoError`] when the artifact cannot be read or fails validation.
pub fn load_snapshot_phase_timings(
    path: &Path,
    options: SnapshotLoadOptions,
) -> Result<SnapshotLoadPhaseTimings, SnapshotIoError> {
    let mut timings = SnapshotLoadPhaseTimings::default();
    let _loaded = load_snapshot_file_inner(path, options, Some(&mut timings))?;
    Ok(timings)
}

fn load_snapshot_file_inner(
    path: &Path,
    options: SnapshotLoadOptions,
    mut timings: Option<&mut SnapshotLoadPhaseTimings>,
) -> Result<LoadedSnapshot, SnapshotIoError> {
    validate_load_options(options)?;
    let bytes = read_decode_payload(path, options, &mut timings)?;
    let reader = parse_reader(&bytes, options, &mut timings)?;
    let phase_start = Instant::now();
    let (configs_vec, schema, schema_hash) = compile_snapshot_schema(&reader)?;
    record_phase(
        &mut timings,
        |timings, elapsed| {
            timings.schema_parse_compile = elapsed;
        },
        phase_start,
    );
    let relationships = decode_relationships_with_optional_timings(
        &reader,
        options.profile,
        options.validation,
        timings.as_deref_mut(),
    )?;
    let phase_start = Instant::now();
    let configs = configs_vec
        .into_iter()
        .map(|config| (config.name.clone(), config))
        .collect::<HashMap<_, _>>();
    let loaded = LoadedSnapshot {
        configs,
        schema,
        relationships,
        revision: reader.header().created_revision,
        schema_hash,
    };
    record_phase(
        &mut timings,
        |timings, elapsed| {
            timings.publish = elapsed;
        },
        phase_start,
    );
    Ok(loaded)
}

fn validate_load_options(options: SnapshotLoadOptions) -> Result<(), SnapshotIoError> {
    if options.validation == SnapshotValidationMode::TrustedFastLoad
        && options.profile != SnapshotLoadProfile::FastLoad
    {
        return Err(SnapshotIoError::UnsupportedOption {
            option: "trusted validation with latency profile",
        });
    }
    if options.integrity == SnapshotIntegrityMode::External
        && options.validation != SnapshotValidationMode::TrustedFastLoad
    {
        return Err(SnapshotIoError::UnsupportedOption {
            option: "external integrity without trusted validation",
        });
    }
    Ok(())
}

fn read_decode_payload(
    path: &Path,
    options: SnapshotLoadOptions,
    timings: &mut Option<&mut SnapshotLoadPhaseTimings>,
) -> Result<Vec<u8>, SnapshotIoError> {
    if options.compression == SnapshotCompression::Zstd {
        return read_decode_zstd_payload(path, options.max_file_bytes, timings);
    }

    let phase_start = Instant::now();
    let bytes = read_capped_file(path, options.max_file_bytes)?;
    record_phase(
        timings,
        |timings, elapsed| timings.file_read = elapsed,
        phase_start,
    );
    let phase_start = Instant::now();
    let bytes = decode_payload(bytes, options)?;
    record_phase(
        timings,
        |timings, elapsed| {
            timings.decompression = elapsed;
        },
        phase_start,
    );
    Ok(bytes)
}

fn read_decode_zstd_payload(
    path: &Path,
    max_file_bytes: NonZeroU64,
    timings: &mut Option<&mut SnapshotLoadPhaseTimings>,
) -> Result<Vec<u8>, SnapshotIoError> {
    let phase_start = Instant::now();
    let (file, metadata_len) = open_capped_file(path, max_file_bytes)?;
    record_phase(
        timings,
        |timings, elapsed| timings.file_read = elapsed,
        phase_start,
    );
    let phase_start = Instant::now();
    let bytes = decode_zstd_bounded(BufReader::new(file.take(metadata_len)), max_file_bytes)?;
    record_phase(
        timings,
        |timings, elapsed| {
            timings.decompression = elapsed;
        },
        phase_start,
    );
    Ok(bytes)
}

fn parse_reader<'a>(
    bytes: &'a [u8],
    options: SnapshotLoadOptions,
    timings: &mut Option<&mut SnapshotLoadPhaseTimings>,
) -> Result<SnapshotReader<'a>, SnapshotIoError> {
    let phase_start = Instant::now();
    let reader = SnapshotReader::parse(bytes, options.integrity, timings.as_deref_mut())?;
    if !reader
        .header()
        .index_profile
        .satisfies(options.required_index_profile)
    {
        return Err(SnapshotIoError::UnsupportedOption {
            option: "snapshot index profile does not satisfy load requirements",
        });
    }
    record_phase(
        timings,
        |timings, elapsed| {
            timings.header_and_sections = elapsed.saturating_sub(timings.checksum);
        },
        phase_start,
    );
    Ok(reader)
}

fn compile_snapshot_schema(
    reader: &SnapshotReader<'_>,
) -> Result<(Vec<NamespaceConfig>, CompiledSchema, SchemaHash), SnapshotIoError> {
    let schema_section = reader.section(SectionKind::Schema)?;
    if schema_section.bytes().len() > MAX_SCHEMA_BYTES {
        return Err(SnapshotIoError::LimitExceeded {
            component: "schema section",
        });
    }
    let schema_source =
        std::str::from_utf8(schema_section.bytes()).map_err(|_| SnapshotIoError::Format {
            reason: "schema section is not valid utf-8",
        })?;
    let configs_vec = crate::parser::parse_dsl(schema_source)
        .map_err(|source| SnapshotIoError::Schema { source })?;
    let schema = schema::compile_legacy_configs(configs_vec.clone())
        .map_err(|source| SnapshotIoError::Schema { source })?;
    let schema_hash = SchemaHash::for_schema(&schema);
    if schema_hash != reader.header().schema_hash {
        return Err(SnapshotIoError::Format {
            reason: "schema hash does not match schema section",
        });
    }
    Ok((configs_vec, schema, schema_hash))
}

fn decode_relationships_with_optional_timings(
    reader: &SnapshotReader<'_>,
    profile: SnapshotLoadProfile,
    validation: SnapshotValidationMode,
    timings: Option<&mut SnapshotLoadPhaseTimings>,
) -> Result<Arc<RelationshipStoreView>, SnapshotIoError> {
    #[cfg(feature = "bench-internals")]
    if let Some(timings) = timings {
        let store = Arc::new(
            IndexedRelationshipStore::decode_snapshot_sections_with_timings(
                reader, profile, validation, timings,
            )?,
        );
        return Ok(Arc::new(RelationshipStoreView::from_checkpoint(store)));
    }
    let _ = timings;
    let store = Arc::new(IndexedRelationshipStore::decode_snapshot_sections(
        reader, profile, validation,
    )?);
    Ok(Arc::new(RelationshipStoreView::from_checkpoint(store)))
}

fn record_phase(
    timings: &mut Option<&mut SnapshotLoadPhaseTimings>,
    record: impl FnOnce(&mut SnapshotLoadPhaseTimings, Duration),
    phase_start: Instant,
) {
    if let Some(timings) = timings.as_deref_mut() {
        record(timings, phase_start.elapsed());
    }
}

fn validate_compression_options(options: SnapshotSaveOptions) -> Result<(), SnapshotIoError> {
    if options.compression == SnapshotCompression::Zstd
        && !zstd::compression_level_range().contains(&options.zstd_level)
    {
        return Err(SnapshotIoError::UnsupportedOption {
            option: "zstd_level",
        });
    }
    Ok(())
}

fn encode_payload(
    bytes: Vec<u8>,
    options: SnapshotSaveOptions,
) -> Result<Vec<u8>, SnapshotIoError> {
    match options.compression {
        SnapshotCompression::None => Ok(bytes),
        SnapshotCompression::Zstd => {
            zstd::stream::encode_all(bytes.as_slice(), options.zstd_level).map_err(Into::into)
        }
    }
}

fn decode_payload(
    bytes: Vec<u8>,
    options: SnapshotLoadOptions,
) -> Result<Vec<u8>, SnapshotIoError> {
    match options.compression {
        SnapshotCompression::None => Ok(bytes),
        SnapshotCompression::Zstd => decode_zstd_bounded(bytes.as_slice(), options.max_file_bytes),
    }
}

fn decode_zstd_bounded<R>(reader: R, max_file_bytes: NonZeroU64) -> Result<Vec<u8>, SnapshotIoError>
where
    R: Read,
{
    let read_limit = max_file_bytes
        .get()
        .checked_add(1)
        .ok_or(SnapshotIoError::LimitExceeded {
            component: "decompressed snapshot file",
        })?;
    let decoder = zstd::stream::read::Decoder::new(reader)?;
    let mut limited_decoder = decoder.take(read_limit);
    let mut output = Vec::new();
    limited_decoder.read_to_end(&mut output)?;
    if checked_u64_from_usize(output.len())? > max_file_bytes.get() {
        return Err(SnapshotIoError::LimitExceeded {
            component: "decompressed snapshot file",
        });
    }
    Ok(output)
}

fn read_capped_file(path: &Path, max_file_bytes: NonZeroU64) -> Result<Vec<u8>, SnapshotIoError> {
    let (file, metadata_len) = open_capped_file(path, max_file_bytes)?;
    let mut bytes = Vec::with_capacity(checked_usize_from_u64(metadata_len)?);
    file.take(metadata_len).read_to_end(&mut bytes)?;
    if checked_u64_from_usize(bytes.len())? != metadata_len {
        return Err(SnapshotIoError::Format {
            reason: "snapshot file changed during read",
        });
    }
    Ok(bytes)
}

fn open_capped_file(
    path: &Path,
    max_file_bytes: NonZeroU64,
) -> Result<(File, u64), SnapshotIoError> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(SnapshotIoError::Format {
            reason: "snapshot path is not a regular file",
        });
    }
    let metadata_len = metadata.len();
    if metadata_len > max_file_bytes.get() {
        return Err(SnapshotIoError::LimitExceeded {
            component: "snapshot file",
        });
    }
    Ok((file, metadata_len))
}

fn write_uncompressed_snapshot_file(
    path: &Path,
    snapshot: &crate::revision::PublishedSnapshot,
    writer: SnapshotSectionWriter,
    index_profile: IndexProfile,
) -> Result<(), SnapshotIoError> {
    let tmp_path = snapshot_tmp_path(path, snapshot.revision());
    let result = write_uncompressed_snapshot_file_inner(&tmp_path, snapshot, writer, index_profile)
        .and_then(|()| fs::rename(&tmp_path, path).map_err(Into::into));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn write_uncompressed_snapshot_file_inner(
    path: &Path,
    snapshot: &crate::revision::PublishedSnapshot,
    writer: SnapshotSectionWriter,
    index_profile: IndexProfile,
) -> Result<(), SnapshotIoError> {
    let relationship_count =
        checked_u32_from_u64(writer.row_count(SectionKind::RelationshipRows)?)?;
    let symbol_count = checked_u32_from_u64(writer.row_count(SectionKind::SymbolTable)?)?;
    let sections = snapshot_sections_with_footer(writer)?;
    let directory = section_directory(&sections)?;
    let file_len = directory_file_len(&directory)?;

    let mut header = Vec::with_capacity(HEADER_LEN);
    write_header(
        &mut header,
        snapshot.schema_hash(),
        relationship_count,
        symbol_count,
        snapshot.revision(),
        file_len,
        index_profile,
    );
    let mut directory_bytes =
        Vec::with_capacity(checked_mul_usize(directory.len(), DIRECTORY_ENTRY_LEN)?);
    for entry in &directory {
        write_directory_entry(&mut directory_bytes, entry);
    }

    let file = File::create(path)?;
    let mut file = BufWriter::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut written = 0_u64;
    write_hashed(&mut file, &mut hasher, &mut written, &header)?;
    write_hashed(&mut file, &mut hasher, &mut written, &directory_bytes)?;
    for section in &sections {
        if section.kind == SectionKind::Footer {
            let digest = hasher.finalize();
            file.write_all(digest.as_bytes())?;
            written = written
                .checked_add(FOOTER_LEN as u64)
                .ok_or(SnapshotIoError::Format {
                    reason: "snapshot writer length overflowed",
                })?;
        } else {
            write_hashed(&mut file, &mut hasher, &mut written, &section.bytes)?;
        }
    }
    file.flush()?;
    if written != file_len {
        return Err(SnapshotIoError::Format {
            reason: "snapshot writer length mismatch",
        });
    }
    Ok(())
}

fn snapshot_sections_with_footer(
    writer: SnapshotSectionWriter,
) -> Result<Vec<SectionPayload>, SnapshotIoError> {
    let mut sections = writer.into_sorted_sections();
    sections.push(SectionPayload {
        kind: SectionKind::Footer,
        flags: 0,
        bytes: vec![0; FOOTER_LEN],
        row_count: 1,
    });
    if sections.len() != REQUIRED_SECTION_COUNT {
        return Err(SnapshotIoError::Format {
            reason: "snapshot writer did not produce all required sections",
        });
    }
    Ok(sections)
}

fn section_directory(
    sections: &[SectionPayload],
) -> Result<Vec<SectionDirectoryEntry>, SnapshotIoError> {
    let directory_len = checked_mul_usize(sections.len(), DIRECTORY_ENTRY_LEN)?;
    let mut next_offset = checked_add_usize(HEADER_LEN, directory_len)?;
    let mut directory = Vec::with_capacity(sections.len());
    for section in sections {
        let len = section.bytes.len();
        directory.push(SectionDirectoryEntry {
            kind: section.kind,
            flags: section.flags,
            offset: checked_u64_from_usize(next_offset)?,
            len: checked_u64_from_usize(len)?,
            row_count: section.row_count,
        });
        next_offset = checked_add_usize(next_offset, len)?;
    }
    Ok(directory)
}

fn directory_file_len(directory: &[SectionDirectoryEntry]) -> Result<u64, SnapshotIoError> {
    let last = directory.last().ok_or(SnapshotIoError::Format {
        reason: "snapshot directory is empty",
    })?;
    last.offset
        .checked_add(last.len)
        .ok_or(SnapshotIoError::Format {
            reason: "snapshot writer length overflowed",
        })
}

fn write_hashed(
    file: &mut BufWriter<File>,
    hasher: &mut blake3::Hasher,
    written: &mut u64,
    bytes: &[u8],
) -> Result<(), SnapshotIoError> {
    file.write_all(bytes)?;
    hasher.update(bytes);
    *written = written
        .checked_add(checked_u64_from_usize(bytes.len())?)
        .ok_or(SnapshotIoError::Format {
            reason: "snapshot writer length overflowed",
        })?;
    Ok(())
}

fn snapshot_tmp_path(path: &Path, revision: Revision) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map_or_else(|| OsString::from("snapshot"), OsString::from);
    file_name.push(format!(".{}.{}.tmp", std::process::id(), revision.get()));
    path.with_file_name(file_name)
}

fn encode_file(
    snapshot: &crate::revision::PublishedSnapshot,
    writer: SnapshotSectionWriter,
    index_profile: IndexProfile,
) -> Result<Vec<u8>, SnapshotIoError> {
    let relationship_count =
        checked_u32_from_u64(writer.row_count(SectionKind::RelationshipRows)?)?;
    let symbol_count = checked_u32_from_u64(writer.row_count(SectionKind::SymbolTable)?)?;
    let sections = snapshot_sections_with_footer(writer)?;
    let directory = section_directory(&sections)?;
    let file_len = directory_file_len(&directory)?;
    let file_len_usize = checked_usize_from_u64(file_len)?;
    let mut bytes = Vec::with_capacity(file_len_usize);
    write_header(
        &mut bytes,
        snapshot.schema_hash(),
        relationship_count,
        symbol_count,
        snapshot.revision(),
        file_len,
        index_profile,
    );
    for entry in &directory {
        write_directory_entry(&mut bytes, entry);
    }
    for section in &sections {
        bytes.extend_from_slice(&section.bytes);
    }
    if bytes.len() != file_len_usize {
        return Err(SnapshotIoError::Format {
            reason: "snapshot writer length mismatch",
        });
    }
    let footer = directory
        .iter()
        .find(|entry| entry.kind == SectionKind::Footer)
        .ok_or(SnapshotIoError::Format {
            reason: "missing footer section",
        })?;
    let footer_offset = checked_usize_from_u64(footer.offset)?;
    let digest = blake3::hash(bytes.get(..footer_offset).ok_or(SnapshotIoError::Format {
        reason: "footer offset is invalid",
    })?);
    let footer_end = checked_add_usize(footer_offset, FOOTER_LEN)?;
    let footer_bytes = bytes
        .get_mut(footer_offset..footer_end)
        .ok_or(SnapshotIoError::Format {
            reason: "footer range is invalid",
        })?;
    footer_bytes.copy_from_slice(digest.as_bytes());
    Ok(bytes)
}

fn write_header(
    target: &mut Vec<u8>,
    schema_hash: SchemaHash,
    relationship_count: u32,
    symbol_count: u32,
    revision: Revision,
    file_len: u64,
    index_profile: IndexProfile,
) {
    target.extend_from_slice(&CURRENT_FORMAT_VERSION.magic());
    target.extend_from_slice(&CURRENT_FORMAT_VERSION.raw().to_le_bytes());
    target.extend_from_slice(&index_profile.flag_bits().to_le_bytes());
    target.extend_from_slice(&HEADER_LEN_U32.to_le_bytes());
    target.extend_from_slice(&REQUIRED_SECTION_COUNT_U32.to_le_bytes());
    target.extend_from_slice(&file_len.to_le_bytes());
    target.extend_from_slice(schema_hash.as_bytes());
    target.extend_from_slice(&relationship_count.to_le_bytes());
    target.extend_from_slice(&symbol_count.to_le_bytes());
    target.extend_from_slice(&revision.get().to_le_bytes());
}

fn write_directory_entry(target: &mut Vec<u8>, entry: &SectionDirectoryEntry) {
    target.extend_from_slice(&entry.kind.raw().to_le_bytes());
    target.extend_from_slice(&entry.flags.to_le_bytes());
    target.extend_from_slice(&entry.offset.to_le_bytes());
    target.extend_from_slice(&entry.len.to_le_bytes());
    target.extend_from_slice(&entry.row_count.to_le_bytes());
}

fn parse_header(bytes: &[u8]) -> Result<SnapshotHeader, SnapshotIoError> {
    let mut cursor = BinaryCursor::new(bytes.get(..HEADER_LEN).ok_or(SnapshotIoError::Format {
        reason: "snapshot header is truncated",
    })?);
    let magic = cursor.read_array::<8>()?;
    let version = cursor.read_u16()?;
    let format_version = SnapshotFormatVersion::from_header(magic, version)?;
    let index_profile = IndexProfile::from_flag_bits(cursor.read_u16()?)?;
    let header_len = cursor.read_u32()?;
    if header_len != HEADER_LEN_U32 {
        return Err(SnapshotIoError::Format {
            reason: "snapshot header length is unsupported",
        });
    }
    let section_count = cursor.read_u32()?;
    if section_count != REQUIRED_SECTION_COUNT_U32 {
        return Err(SnapshotIoError::Format {
            reason: "snapshot section count is unsupported",
        });
    }
    let file_len = cursor.read_u64()?;
    let actual_len = checked_u64_from_usize(bytes.len())?;
    if file_len != actual_len {
        return Err(SnapshotIoError::Format {
            reason: "snapshot file length does not match header",
        });
    }
    let schema_hash = SchemaHash::from_bytes(cursor.read_array::<32>()?);
    let relationship_count = cursor.read_u32()?;
    let symbol_count = cursor.read_u32()?;
    let revision_raw = cursor.read_u64()?;
    let revision =
        NonZeroU64::new(revision_raw)
            .map(Revision::new)
            .ok_or(SnapshotIoError::Format {
                reason: "snapshot revision must be non-zero",
            })?;
    Ok(SnapshotHeader {
        format_version,
        schema_hash,
        relationship_count,
        symbol_count,
        created_revision: revision,
        index_profile,
    })
}

fn parse_section_count(bytes: &[u8]) -> Result<usize, SnapshotIoError> {
    let start = 16;
    let end = checked_add_usize(start, 4)?;
    let count_bytes = bytes.get(start..end).ok_or(SnapshotIoError::Format {
        reason: "snapshot section count is missing",
    })?;
    let mut array = [0_u8; 4];
    array.copy_from_slice(count_bytes);
    checked_usize_from_u32(u32::from_le_bytes(array))
}

fn validate_section_flags(
    version: SnapshotFormatVersion,
    kind: SectionKind,
    flags: u16,
) -> Result<(), SnapshotIoError> {
    if flags == 0 {
        return Ok(());
    }
    if version == SnapshotFormatVersion::V3
        && matches!(
            kind,
            SectionKind::SymbolTable | SectionKind::RelationshipRows | SectionKind::SymbolLookup
        )
    {
        return Ok(());
    }
    Err(SnapshotIoError::Format {
        reason: "section flags are unsupported",
    })
}

fn validate_required_sections(sections: &SnapshotSections<'_>) -> Result<(), SnapshotIoError> {
    for kind in [
        SectionKind::Schema,
        SectionKind::SymbolBytes,
        SectionKind::SymbolTable,
        SectionKind::RelationshipRows,
        SectionKind::IndexDirectory,
        SectionKind::IndexKeys,
        SectionKind::PostingRanges,
        SectionKind::PostingRowIds,
        SectionKind::SymbolHashes,
        SectionKind::SymbolLookup,
        SectionKind::Footer,
    ] {
        if sections
            .get(section_slot(kind))
            .copied()
            .flatten()
            .is_none()
        {
            return Err(SnapshotIoError::Format {
                reason: "missing required snapshot section",
            });
        }
    }
    Ok(())
}

fn validate_non_overlapping_sections(
    entries: &mut [(u64, u64, SectionKind)],
) -> Result<(), SnapshotIoError> {
    entries.sort_by_key(|(offset, _, _)| *offset);
    let mut previous_end = 0_u64;
    for (offset, len, _) in entries {
        if *offset < previous_end {
            return Err(SnapshotIoError::Format {
                reason: "snapshot sections overlap",
            });
        }
        previous_end = offset.checked_add(*len).ok_or(SnapshotIoError::Format {
            reason: "snapshot section range overflowed",
        })?;
    }
    Ok(())
}

fn validate_footer(
    bytes: &[u8],
    sections: &SnapshotSections<'_>,
    integrity: SnapshotIntegrityMode,
) -> Result<(), SnapshotIoError> {
    let footer = sections
        .get(section_slot(SectionKind::Footer))
        .copied()
        .flatten()
        .ok_or(SnapshotIoError::Format {
            reason: "missing footer section",
        })?;
    if footer.bytes().len() != FOOTER_LEN {
        return Err(SnapshotIoError::Format {
            reason: "footer length is invalid",
        });
    }
    let footer_start = bytes
        .len()
        .checked_sub(FOOTER_LEN)
        .ok_or(SnapshotIoError::Format {
            reason: "footer range is invalid",
        })?;
    let expected_footer = bytes.get(footer_start..).ok_or(SnapshotIoError::Format {
        reason: "footer range is invalid",
    })?;
    if expected_footer != footer.bytes() {
        return Err(SnapshotIoError::Format {
            reason: "footer must be the final section",
        });
    }
    if integrity == SnapshotIntegrityMode::External {
        return Ok(());
    }
    let digest = blake3::hash(bytes.get(..footer_start).ok_or(SnapshotIoError::Format {
        reason: "footer offset is invalid",
    })?);
    if digest.as_bytes().as_slice() != footer.bytes() {
        return Err(SnapshotIoError::Format {
            reason: "snapshot checksum mismatch",
        });
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct BinaryCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> BinaryCursor<'a> {
    /// Creates a cursor over little-endian binary data.
    #[must_use]
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Reads a fixed-size byte array.
    pub(crate) fn read_array<const N: usize>(&mut self) -> Result<[u8; N], SnapshotIoError> {
        let end = checked_add_usize(self.offset, N)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(SnapshotIoError::Format {
                reason: "snapshot section is truncated",
            })?;
        let mut array = [0_u8; N];
        array.copy_from_slice(slice);
        self.offset = end;
        Ok(array)
    }

    /// Reads a little-endian `u16`.
    pub(crate) fn read_u16(&mut self) -> Result<u16, SnapshotIoError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    /// Reads a little-endian `u32`.
    pub(crate) fn read_u32(&mut self) -> Result<u32, SnapshotIoError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    /// Reads a little-endian `u64`.
    pub(crate) fn read_u64(&mut self) -> Result<u64, SnapshotIoError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    /// Returns true when no unread bytes remain.
    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

/// Converts a little-endian u32 count to usize.
pub(crate) fn checked_usize_from_u32(value: u32) -> Result<usize, SnapshotIoError> {
    usize::try_from(value).map_err(|_| SnapshotIoError::LimitExceeded {
        component: "snapshot u32 count",
    })
}

/// Converts a little-endian u64 count to usize.
pub(crate) fn checked_usize_from_u64(value: u64) -> Result<usize, SnapshotIoError> {
    usize::try_from(value).map_err(|_| SnapshotIoError::LimitExceeded {
        component: "snapshot u64 count",
    })
}

/// Converts a usize count to u32.
pub(crate) fn checked_u32_from_usize(value: usize) -> Result<u32, SnapshotIoError> {
    u32::try_from(value).map_err(|_| SnapshotIoError::LimitExceeded {
        component: "snapshot u32 count",
    })
}

fn checked_u32_from_u64(value: u64) -> Result<u32, SnapshotIoError> {
    u32::try_from(value).map_err(|_| SnapshotIoError::LimitExceeded {
        component: "snapshot u32 count",
    })
}

fn checked_u64_from_usize(value: usize) -> Result<u64, SnapshotIoError> {
    u64::try_from(value).map_err(|_| SnapshotIoError::LimitExceeded {
        component: "snapshot u64 count",
    })
}

/// Checked `usize` addition for snapshot parsing.
pub(crate) fn checked_add_usize(left: usize, right: usize) -> Result<usize, SnapshotIoError> {
    left.checked_add(right).ok_or(SnapshotIoError::Format {
        reason: "snapshot offset overflowed",
    })
}

/// Checked `usize` multiplication for snapshot parsing.
pub(crate) fn checked_mul_usize(left: usize, right: usize) -> Result<usize, SnapshotIoError> {
    left.checked_mul(right).ok_or(SnapshotIoError::Format {
        reason: "snapshot length overflowed",
    })
}

/// Validates that `value` is unique in `seen`.
pub(crate) fn insert_unique<T>(
    seen: &mut HashSet<T>,
    value: T,
    reason: &'static str,
) -> Result<(), SnapshotIoError>
where
    T: Eq + std::hash::Hash,
{
    if seen.insert(value) {
        Ok(())
    } else {
        Err(SnapshotIoError::Format { reason })
    }
}

fn non_zero_u64(value: u64) -> NonZeroU64 {
    match NonZeroU64::new(value) {
        Some(value) => value,
        None => NonZeroU64::MIN,
    }
}

pub(crate) fn one_snapshot_retention() -> NonZeroUsize {
    match NonZeroUsize::new(1) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}
