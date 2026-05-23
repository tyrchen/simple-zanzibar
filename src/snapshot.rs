//! Versioned compact snapshot artifact save/load support.
//!
//! Snapshot files are deployment artifacts for prebuilt local authorization data. They are treated
//! as untrusted input during load: headers, section bounds, symbol ids, row ids, index order, and
//! checksums are validated before a service publishes the loaded snapshot.

use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Write},
    fs::{self, File},
    io::{self, Read},
    num::{NonZeroU64, NonZeroUsize},
    path::Path,
    sync::Arc,
};

use thiserror::Error;

use crate::{
    domain::DomainError,
    error::ZanzibarError,
    model::{NamespaceConfig, RelationConfig, UsersetExpression},
    relationship::{IndexedRelationshipStore, StoreError},
    revision::{Revision, SchemaHash},
    schema::{self, CompiledSchema},
};

const MAGIC: [u8; 8] = *b"SZSNAP\0\x01";
const FORMAT_VERSION: u16 = 1;
const HEADER_LEN: usize = 76;
const HEADER_LEN_U32: u32 = 76;
const DIRECTORY_ENTRY_LEN: usize = 28;
const FOOTER_LEN: usize = 32;
const REQUIRED_SECTION_COUNT: usize = 9;
const REQUIRED_SECTION_COUNT_U32: u32 = 9;
const MAX_SCHEMA_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Options used when saving a compact snapshot artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotSaveOptions {
    /// Snapshot compression mode. Version 1 supports only uncompressed snapshots.
    pub compression: SnapshotCompression,
    /// Whether serialized lookup indexes are included.
    ///
    /// Version 1 requires indexes because fast loading depends on stable sorted index arrays.
    pub include_indexes: bool,
}

impl Default for SnapshotSaveOptions {
    fn default() -> Self {
        Self {
            compression: SnapshotCompression::None,
            include_indexes: true,
        }
    }
}

/// Snapshot compression mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotCompression {
    /// Store all sections uncompressed.
    None,
}

/// Options used when loading a compact snapshot artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLoadOptions {
    /// Runtime index profile to construct from serialized sorted arrays.
    pub profile: SnapshotLoadProfile,
    /// Maximum accepted file size in bytes.
    pub max_file_bytes: NonZeroU64,
}

impl Default for SnapshotLoadOptions {
    fn default() -> Self {
        Self {
            profile: SnapshotLoadProfile::FastLoad,
            max_file_bytes: non_zero_u64(DEFAULT_MAX_FILE_BYTES),
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
    pub(crate) relationships: Arc<IndexedRelationshipStore>,
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
    Footer = 9,
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
            9 => Ok(Self::Footer),
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
    pub(crate) schema_hash: SchemaHash,
    pub(crate) relationship_count: u32,
    pub(crate) symbol_count: u32,
    pub(crate) created_revision: Revision,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotSection<'a> {
    bytes: &'a [u8],
    row_count: u64,
}

impl<'a> SnapshotSection<'a> {
    /// Returns the raw section bytes.
    #[must_use]
    pub(crate) const fn bytes(self) -> &'a [u8] {
        self.bytes
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
    sections: HashMap<SectionKind, SnapshotSection<'a>>,
}

impl<'a> SnapshotReader<'a> {
    /// Parses and validates the outer snapshot envelope.
    pub(crate) fn parse(bytes: &'a [u8]) -> Result<Self, SnapshotIoError> {
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
        let mut sections = HashMap::with_capacity(section_count);
        for _ in 0..section_count {
            let raw_kind = cursor.read_u16()?;
            let kind = SectionKind::from_raw(raw_kind)?;
            let flags = cursor.read_u16()?;
            if flags != 0 {
                return Err(SnapshotIoError::Format {
                    reason: "section flags are unsupported",
                });
            }
            let offset = cursor.read_u64()?;
            let len = cursor.read_u64()?;
            let row_count = cursor.read_u64()?;
            if sections.contains_key(&kind) {
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
            sections.insert(
                kind,
                SnapshotSection {
                    bytes: section_bytes,
                    row_count,
                },
            );
            entries.push((offset, len, kind));
        }

        validate_required_sections(&sections)?;
        validate_non_overlapping_sections(&mut entries)?;
        validate_footer(bytes, &sections)?;
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
            .get(&kind)
            .copied()
            .ok_or(SnapshotIoError::Format {
                reason: "missing required snapshot section",
            })
    }
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
        if self.sections.iter().any(|section| section.kind == kind) {
            return Err(SnapshotIoError::Format {
                reason: "duplicate snapshot section",
            });
        }
        self.sections.push(SectionPayload {
            kind,
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
    bytes: Vec<u8>,
    row_count: u64,
}

#[derive(Debug)]
struct SectionDirectoryEntry {
    kind: SectionKind,
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

    let mut writer = SnapshotSectionWriter::default();
    let schema_source = canonical_schema_source(snapshot.configs())?;
    if schema_source.len() > MAX_SCHEMA_BYTES {
        return Err(SnapshotIoError::LimitExceeded {
            component: "schema section",
        });
    }
    writer.add_section(SectionKind::Schema, schema_source.into_bytes(), 1)?;
    snapshot
        .relationships()
        .encode_snapshot_sections(&mut writer)?;
    let bytes = encode_file(snapshot, writer)?;
    fs::write(path, bytes)?;
    Ok(())
}

pub(crate) fn load_snapshot_file(
    path: &Path,
    options: SnapshotLoadOptions,
) -> Result<LoadedSnapshot, SnapshotIoError> {
    let bytes = read_capped_file(path, options.max_file_bytes)?;
    let reader = SnapshotReader::parse(&bytes)?;
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
    let relationships = Arc::new(IndexedRelationshipStore::decode_snapshot_sections(
        &reader,
        options.profile,
    )?);
    let configs = configs_vec
        .into_iter()
        .map(|config| (config.name.clone(), config))
        .collect::<HashMap<_, _>>();
    Ok(LoadedSnapshot {
        configs,
        schema,
        relationships,
        revision: reader.header().created_revision,
        schema_hash,
    })
}

fn read_capped_file(path: &Path, max_file_bytes: NonZeroU64) -> Result<Vec<u8>, SnapshotIoError> {
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

    let mut bytes = Vec::with_capacity(checked_usize_from_u64(metadata_len)?);
    file.take(metadata_len).read_to_end(&mut bytes)?;
    if checked_u64_from_usize(bytes.len())? != metadata_len {
        return Err(SnapshotIoError::Format {
            reason: "snapshot file changed during read",
        });
    }
    Ok(bytes)
}

fn encode_file(
    snapshot: &crate::revision::PublishedSnapshot,
    writer: SnapshotSectionWriter,
) -> Result<Vec<u8>, SnapshotIoError> {
    let relationship_count =
        checked_u32_from_u64(writer.row_count(SectionKind::RelationshipRows)?)?;
    let symbol_count = checked_u32_from_u64(writer.row_count(SectionKind::SymbolTable)?)?;
    let mut sections = writer.into_sorted_sections();
    sections.push(SectionPayload {
        kind: SectionKind::Footer,
        bytes: vec![0; FOOTER_LEN],
        row_count: 1,
    });
    if sections.len() != REQUIRED_SECTION_COUNT {
        return Err(SnapshotIoError::Format {
            reason: "snapshot writer did not produce all required sections",
        });
    }

    let directory_len = checked_mul_usize(sections.len(), DIRECTORY_ENTRY_LEN)?;
    let mut next_offset = checked_add_usize(HEADER_LEN, directory_len)?;
    let mut directory = Vec::with_capacity(sections.len());
    for section in &sections {
        let len = section.bytes.len();
        directory.push(SectionDirectoryEntry {
            kind: section.kind,
            offset: checked_u64_from_usize(next_offset)?,
            len: checked_u64_from_usize(len)?,
            row_count: section.row_count,
        });
        next_offset = checked_add_usize(next_offset, len)?;
    }
    let file_len = checked_u64_from_usize(next_offset)?;
    let mut bytes = Vec::with_capacity(next_offset);
    write_header(
        &mut bytes,
        snapshot.schema_hash(),
        relationship_count,
        symbol_count,
        snapshot.revision(),
        file_len,
    );
    for entry in &directory {
        write_directory_entry(&mut bytes, entry);
    }
    for section in &sections {
        bytes.extend_from_slice(&section.bytes);
    }
    if bytes.len() != next_offset {
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
) {
    target.extend_from_slice(&MAGIC);
    target.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    target.extend_from_slice(&0_u16.to_le_bytes());
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
    target.extend_from_slice(&0_u16.to_le_bytes());
    target.extend_from_slice(&entry.offset.to_le_bytes());
    target.extend_from_slice(&entry.len.to_le_bytes());
    target.extend_from_slice(&entry.row_count.to_le_bytes());
}

fn parse_header(bytes: &[u8]) -> Result<SnapshotHeader, SnapshotIoError> {
    let mut cursor = BinaryCursor::new(bytes.get(..HEADER_LEN).ok_or(SnapshotIoError::Format {
        reason: "snapshot header is truncated",
    })?);
    let magic = cursor.read_array::<8>()?;
    if magic != MAGIC {
        return Err(SnapshotIoError::Format {
            reason: "snapshot magic is invalid",
        });
    }
    let version = cursor.read_u16()?;
    if version != FORMAT_VERSION {
        return Err(SnapshotIoError::Format {
            reason: "snapshot format version is unsupported",
        });
    }
    let flags = cursor.read_u16()?;
    if flags != 0 {
        return Err(SnapshotIoError::Format {
            reason: "snapshot flags are unsupported",
        });
    }
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
        schema_hash,
        relationship_count,
        symbol_count,
        created_revision: revision,
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

fn validate_required_sections(
    sections: &HashMap<SectionKind, SnapshotSection<'_>>,
) -> Result<(), SnapshotIoError> {
    for kind in [
        SectionKind::Schema,
        SectionKind::SymbolBytes,
        SectionKind::SymbolTable,
        SectionKind::RelationshipRows,
        SectionKind::IndexDirectory,
        SectionKind::IndexKeys,
        SectionKind::PostingRanges,
        SectionKind::PostingRowIds,
        SectionKind::Footer,
    ] {
        if !sections.contains_key(&kind) {
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
    sections: &HashMap<SectionKind, SnapshotSection<'_>>,
) -> Result<(), SnapshotIoError> {
    let footer = sections
        .get(&SectionKind::Footer)
        .copied()
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

fn canonical_schema_source(
    configs: &HashMap<String, NamespaceConfig>,
) -> Result<String, SnapshotIoError> {
    let mut output = String::new();
    let mut namespaces = configs.values().collect::<Vec<_>>();
    namespaces.sort_by(|left, right| left.name.cmp(&right.name));
    for namespace in namespaces {
        writeln!(&mut output, "namespace {} {{", namespace.name).map_err(format_error)?;
        let mut relations = namespace.relations.values().collect::<Vec<_>>();
        relations.sort_by(|left, right| left.name.0.cmp(&right.name.0));
        for relation in relations {
            write_relation(&mut output, relation)?;
        }
        writeln!(&mut output, "}}\n").map_err(format_error)?;
    }
    Ok(output)
}

fn write_relation(output: &mut String, relation: &RelationConfig) -> Result<(), SnapshotIoError> {
    match &relation.userset_rewrite {
        Some(expression) => {
            writeln!(output, "    relation {} {{", relation.name.0).map_err(format_error)?;
            write!(output, "        rewrite ").map_err(format_error)?;
            write_expression(output, expression)?;
            writeln!(output).map_err(format_error)?;
            writeln!(output, "    }}").map_err(format_error)?;
        }
        None => {
            writeln!(output, "    relation {} {{}}", relation.name.0).map_err(format_error)?;
        }
    }
    Ok(())
}

fn write_expression(
    output: &mut String,
    expression: &UsersetExpression,
) -> Result<(), SnapshotIoError> {
    match expression {
        UsersetExpression::This => write!(output, "this").map_err(format_error),
        UsersetExpression::ComputedUserset { relation } => {
            write!(output, "computed_userset(relation: \"{}\")", relation.0).map_err(format_error)
        }
        UsersetExpression::TupleToUserset {
            tupleset_relation,
            computed_userset_relation,
        } => write!(
            output,
            "tuple_to_userset(tupleset: \"{}\", computed_userset: \"{}\")",
            tupleset_relation.0, computed_userset_relation.0,
        )
        .map_err(format_error),
        UsersetExpression::Union(expressions) => {
            write_expression_list(output, "union", expressions)
        }
        UsersetExpression::Intersection(expressions) => {
            write_expression_list(output, "intersection", expressions)
        }
        UsersetExpression::Exclusion { base, exclude } => {
            write!(output, "exclusion(").map_err(format_error)?;
            write_expression(output, base)?;
            write!(output, ", ").map_err(format_error)?;
            write_expression(output, exclude)?;
            write!(output, ")").map_err(format_error)
        }
    }
}

fn write_expression_list(
    output: &mut String,
    name: &str,
    expressions: &[UsersetExpression],
) -> Result<(), SnapshotIoError> {
    write!(output, "{name}(").map_err(format_error)?;
    let mut separator = "";
    for expression in expressions {
        write!(output, "{separator}").map_err(format_error)?;
        write_expression(output, expression)?;
        separator = ", ";
    }
    write!(output, ")").map_err(format_error)
}

fn format_error(_: fmt::Error) -> SnapshotIoError {
    SnapshotIoError::Format {
        reason: "schema serialization failed",
    }
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
