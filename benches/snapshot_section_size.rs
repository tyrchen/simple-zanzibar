//! Benchmarks and reports compact snapshot section-size composition.

use std::{
    env, fs,
    hint::black_box,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process,
    time::Duration,
};

use criterion::Criterion;
use simple_zanzibar::{
    IndexProfile, SnapshotCompression, SnapshotSaveOptions, ZanzibarEngine, domain::Relationship,
    eval::EvaluationLimits, relationship::RelationshipMutation,
};

const RULES_1M: usize = 1_000_000;
const MUTATION_BATCH_LIMIT: usize = 10_000;
const FIXED_RELATIONSHIP_COUNT: usize = 9;
const TARGET_USER_ID: &str = "target_user";
const EDITOR_USER_ID: &str = "editor_user";
const OWNER_USER_ID: &str = "owner_user";
const HEADER_LEN: usize = 76;
const DIRECTORY_ENTRY_LEN: usize = 28;
const INDEX_DIRECTORY_ENTRY_LEN: usize = 20;
const INDEX_KEY_LEN: u64 = 12;
const POSTING_RANGE_LEN: u64 = 12;
const POSTING_RANGE_LEN_USIZE: usize = 12;
const ROW_ID_LEN: u64 = 4;

fn main() {
    if cfg!(debug_assertions) {
        return;
    }

    let filters = benchmark_filters();
    if !should_benchmark("snapshot_section_size", &filters) {
        return;
    }

    let reports = build_reports();
    print_reports(&reports);

    let mut criterion = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_millis(500))
        .configure_from_args();

    for report in &reports {
        let name = format!("snapshot_section_size/{}/total_bytes", report.label);
        let raw_bytes = report.raw_bytes;
        criterion.bench_function(&name, |bencher| {
            bencher.iter(|| black_box(raw_bytes));
        });
    }
    criterion.final_summary();
}

#[derive(Debug, Clone)]
struct SnapshotSizeReport {
    label: &'static str,
    profile: IndexProfile,
    raw_bytes: u64,
    zstd_bytes: u64,
    relationship_count: u32,
    symbol_count: u32,
    sections: Vec<SectionReport>,
    index_groups: Vec<IndexGroupReport>,
}

impl SnapshotSizeReport {
    fn index_payload_bytes(&self) -> u64 {
        self.section_len(SectionKind::IndexDirectory)
            .saturating_add(self.section_len(SectionKind::IndexKeys))
            .saturating_add(self.section_len(SectionKind::PostingRanges))
            .saturating_add(self.section_len(SectionKind::PostingRowIds))
    }

    fn non_index_payload_bytes(&self) -> u64 {
        self.raw_bytes
            .saturating_sub(snapshot_envelope_bytes())
            .saturating_sub(self.index_payload_bytes())
    }

    fn section_len(&self, kind: SectionKind) -> u64 {
        self.sections
            .iter()
            .find(|section| section.kind == kind)
            .map_or(0, |section| section.len)
    }
}

#[derive(Debug, Clone, Copy)]
struct SectionReport {
    kind: SectionKind,
    len: u64,
    row_count: u64,
}

#[derive(Debug, Clone, Copy)]
struct IndexGroupReport {
    kind: SnapshotIndexKind,
    key_count: u64,
    range_count: u64,
    overflow_row_id_count: u64,
}

impl IndexGroupReport {
    fn key_bytes(self) -> u64 {
        self.key_count.saturating_mul(INDEX_KEY_LEN)
    }

    fn range_bytes(self) -> u64 {
        self.range_count.saturating_mul(POSTING_RANGE_LEN)
    }

    fn overflow_bytes(self) -> u64 {
        self.overflow_row_id_count.saturating_mul(ROW_ID_LEN)
    }

    fn payload_bytes(self) -> u64 {
        self.key_bytes()
            .saturating_add(self.range_bytes())
            .saturating_add(self.overflow_bytes())
    }

    fn total_postings(self) -> u64 {
        self.range_count.saturating_add(self.overflow_row_id_count)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Schema,
    SymbolBytes,
    SymbolTable,
    RelationshipRows,
    IndexDirectory,
    IndexKeys,
    PostingRanges,
    PostingRowIds,
    SymbolHashes,
    SymbolLookup,
    Footer,
}

impl SectionKind {
    fn from_raw(value: u16) -> Result<Self, String> {
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
            _ => Err(format!("unknown section kind {value}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::SymbolBytes => "symbol_bytes",
            Self::SymbolTable => "symbol_table",
            Self::RelationshipRows => "relationship_rows",
            Self::IndexDirectory => "index_directory",
            Self::IndexKeys => "index_keys",
            Self::PostingRanges => "posting_ranges",
            Self::PostingRowIds => "posting_row_ids",
            Self::SymbolHashes => "symbol_hashes",
            Self::SymbolLookup => "symbol_lookup",
            Self::Footer => "footer",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SnapshotIndexKind {
    Resource,
    ResourceObject,
    ResourceTypeRelation,
    ResourceType,
    Subject,
    SubjectTypeRelation,
    SubjectType,
}

impl SnapshotIndexKind {
    fn from_raw(value: u16) -> Result<Self, String> {
        match value {
            1 => Ok(Self::Resource),
            2 => Ok(Self::ResourceObject),
            3 => Ok(Self::ResourceTypeRelation),
            4 => Ok(Self::ResourceType),
            5 => Ok(Self::Subject),
            6 => Ok(Self::SubjectTypeRelation),
            7 => Ok(Self::SubjectType),
            _ => Err(format!("unknown snapshot index kind {value}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Resource => "resource",
            Self::ResourceObject => "resource_object",
            Self::ResourceTypeRelation => "resource_type_relation",
            Self::ResourceType => "resource_type",
            Self::Subject => "subject",
            Self::SubjectTypeRelation => "subject_type_relation",
            Self::SubjectType => "subject_type",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SectionEntry {
    kind: SectionKind,
    offset: u64,
    len: u64,
    row_count: u64,
}

#[derive(Debug, Clone, Copy)]
struct IndexDirectoryEntry {
    kind: SnapshotIndexKind,
    key_count: u32,
    posting_range_start: u32,
    posting_range_count: u32,
}

fn build_reports() -> Vec<SnapshotSizeReport> {
    let engine = build_engine(RULES_1M);
    [
        ("full_1m", IndexProfile::Full),
        ("check_only_1m", IndexProfile::CheckOnly),
        (
            "check_and_object_audit_1m",
            IndexProfile::CheckAndObjectAudit,
        ),
    ]
    .into_iter()
    .map(|(label, profile)| snapshot_size_report(&engine, label, profile))
    .collect()
}

fn snapshot_size_report(
    engine: &ZanzibarEngine,
    label: &'static str,
    profile: IndexProfile,
) -> SnapshotSizeReport {
    let raw_path = unique_snapshot_path(label, "raw");
    let zstd_path = unique_snapshot_path(label, "zstd");
    must(
        engine.save_snapshot(
            &raw_path,
            SnapshotSaveOptions {
                index_profile: profile,
                ..SnapshotSaveOptions::default()
            },
        ),
        "failed to save raw snapshot",
    );
    must(
        engine.save_snapshot(
            &zstd_path,
            SnapshotSaveOptions {
                compression: SnapshotCompression::Zstd,
                index_profile: profile,
                ..SnapshotSaveOptions::default()
            },
        ),
        "failed to save zstd snapshot",
    );

    let raw_bytes = must(fs::read(&raw_path), "failed to read raw snapshot");
    let zstd_len = must(fs::metadata(&zstd_path), "failed to stat zstd snapshot").len();
    let report = must(
        parse_snapshot_report(label, profile, &raw_bytes, zstd_len),
        "failed to parse raw snapshot",
    );
    remove_file(&raw_path);
    remove_file(&zstd_path);
    report
}

fn parse_snapshot_report(
    label: &'static str,
    profile: IndexProfile,
    bytes: &[u8],
    zstd_bytes: u64,
) -> Result<SnapshotSizeReport, String> {
    let relationship_count = read_u32(bytes, 60)?;
    let symbol_count = read_u32(bytes, 64)?;
    let sections = parse_sections(bytes)?;
    let index_groups = parse_index_groups(bytes, &sections)?;
    Ok(SnapshotSizeReport {
        label,
        profile,
        raw_bytes: u64::try_from(bytes.len())
            .map_err(|_| "snapshot length does not fit u64".to_string())?,
        zstd_bytes,
        relationship_count,
        symbol_count,
        sections: sections
            .iter()
            .map(|entry| SectionReport {
                kind: entry.kind,
                len: entry.len,
                row_count: entry.row_count,
            })
            .collect(),
        index_groups,
    })
}

fn parse_sections(bytes: &[u8]) -> Result<Vec<SectionEntry>, String> {
    let header_len = usize::try_from(read_u32(bytes, 12)?)
        .map_err(|_| "header length does not fit usize".to_string())?;
    if header_len != HEADER_LEN {
        return Err("unexpected header length".to_string());
    }
    let section_count = usize::try_from(read_u32(bytes, 16)?)
        .map_err(|_| "section count does not fit usize".to_string())?;
    let directory_len = section_count
        .checked_mul(DIRECTORY_ENTRY_LEN)
        .ok_or("directory length overflowed")?;
    let directory_end = header_len
        .checked_add(directory_len)
        .ok_or("directory end overflowed")?;
    let directory = bytes
        .get(header_len..directory_end)
        .ok_or("section directory is out of bounds")?;
    let mut sections = Vec::with_capacity(section_count);
    for entry in directory.chunks_exact(DIRECTORY_ENTRY_LEN) {
        let kind = SectionKind::from_raw(read_u16(entry, 0)?)?;
        let offset = read_u64(entry, 4)?;
        let len = read_u64(entry, 12)?;
        let row_count = read_u64(entry, 20)?;
        sections.push(SectionEntry {
            kind,
            offset,
            len,
            row_count,
        });
    }
    Ok(sections)
}

fn parse_index_groups(
    bytes: &[u8],
    sections: &[SectionEntry],
) -> Result<Vec<IndexGroupReport>, String> {
    let index_directory = section_slice(bytes, sections, SectionKind::IndexDirectory)?;
    let posting_ranges = section_slice(bytes, sections, SectionKind::PostingRanges)?;
    let mut groups = Vec::new();
    for entry in index_directory.chunks_exact(INDEX_DIRECTORY_ENTRY_LEN) {
        let directory = parse_index_directory_entry(entry)?;
        let overflow_row_id_count = overflow_row_ids_for_group(posting_ranges, directory)?;
        groups.push(IndexGroupReport {
            kind: directory.kind,
            key_count: u64::from(directory.key_count),
            range_count: u64::from(directory.posting_range_count),
            overflow_row_id_count,
        });
    }
    Ok(groups)
}

fn parse_index_directory_entry(bytes: &[u8]) -> Result<IndexDirectoryEntry, String> {
    let _key_start = read_u32(bytes, 4)?;
    Ok(IndexDirectoryEntry {
        kind: SnapshotIndexKind::from_raw(read_u16(bytes, 0)?)?,
        key_count: read_u32(bytes, 8)?,
        posting_range_start: read_u32(bytes, 12)?,
        posting_range_count: read_u32(bytes, 16)?,
    })
}

fn overflow_row_ids_for_group(
    posting_ranges: &[u8],
    directory: IndexDirectoryEntry,
) -> Result<u64, String> {
    let range_start = usize::try_from(directory.posting_range_start)
        .map_err(|_| "range start does not fit usize".to_string())?;
    let range_count = usize::try_from(directory.posting_range_count)
        .map_err(|_| "range count does not fit usize".to_string())?;
    let start = range_start
        .checked_mul(POSTING_RANGE_LEN_USIZE)
        .ok_or("range byte start overflowed")?;
    let len = range_count
        .checked_mul(POSTING_RANGE_LEN_USIZE)
        .ok_or("range byte length overflowed")?;
    let end = start.checked_add(len).ok_or("range byte end overflowed")?;
    let ranges = posting_ranges
        .get(start..end)
        .ok_or("posting range span is out of bounds")?;
    ranges
        .chunks_exact(POSTING_RANGE_LEN_USIZE)
        .try_fold(0_u64, |total, range| {
            total
                .checked_add(u64::from(read_u32(range, 8)?))
                .ok_or_else(|| "overflow row id count overflowed".to_string())
        })
}

fn section_slice<'a>(
    bytes: &'a [u8],
    sections: &[SectionEntry],
    kind: SectionKind,
) -> Result<&'a [u8], String> {
    let section = sections
        .iter()
        .find(|section| section.kind == kind)
        .ok_or_else(|| format!("missing section {}", kind.name()))?;
    let start = usize::try_from(section.offset)
        .map_err(|_| format!("section {} offset does not fit usize", kind.name()))?;
    let len = usize::try_from(section.len)
        .map_err(|_| format!("section {} length does not fit usize", kind.name()))?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| format!("section {} range overflowed", kind.name()))?;
    bytes
        .get(start..end)
        .ok_or_else(|| format!("section {} is out of bounds", kind.name()))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset.checked_add(2).ok_or("u16 offset overflowed")?;
    let slice = bytes.get(offset..end).ok_or("u16 range is invalid")?;
    let mut value = [0_u8; 2];
    value.copy_from_slice(slice);
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("u32 offset overflowed")?;
    let slice = bytes.get(offset..end).ok_or("u32 range is invalid")?;
    let mut value = [0_u8; 4];
    value.copy_from_slice(slice);
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let end = offset.checked_add(8).ok_or("u64 offset overflowed")?;
    let slice = bytes.get(offset..end).ok_or("u64 range is invalid")?;
    let mut value = [0_u8; 8];
    value.copy_from_slice(slice);
    Ok(u64::from_le_bytes(value))
}

fn print_reports(reports: &[SnapshotSizeReport]) {
    for report in reports {
        eprintln!(
            "snapshot_section_size/{}: profile={:?} raw={} zstd={} relationships={} symbols={} \
             index_payload={} non_index_payload={}",
            report.label,
            report.profile,
            report.raw_bytes,
            report.zstd_bytes,
            report.relationship_count,
            report.symbol_count,
            report.index_payload_bytes(),
            report.non_index_payload_bytes(),
        );
        for section in &report.sections {
            eprintln!(
                "snapshot_section_size/{}/section/{}: bytes={} rows={}",
                report.label,
                section.kind.name(),
                section.len,
                section.row_count,
            );
        }
        for group in &report.index_groups {
            eprintln!(
                "snapshot_section_size/{}/index_group/{}: keys={} ranges={} overflow_row_ids={} \
                 total_postings={} payload_bytes={} key_bytes={} range_bytes={} overflow_bytes={}",
                report.label,
                group.kind.name(),
                group.key_count,
                group.range_count,
                group.overflow_row_id_count,
                group.total_postings(),
                group.payload_bytes(),
                group.key_bytes(),
                group.range_bytes(),
                group.overflow_bytes(),
            );
        }
    }

    if let Some(full) = reports
        .iter()
        .find(|report| report.profile == IndexProfile::Full)
    {
        for report in reports
            .iter()
            .filter(|report| report.profile != IndexProfile::Full)
        {
            let saved = full.raw_bytes.saturating_sub(report.raw_bytes);
            let saved_percent = percent_string(saved, full.raw_bytes);
            eprintln!(
                "snapshot_section_size/comparison/{:?}: saved_bytes={} \
                 saved_percent={saved_percent}",
                report.profile, saved,
            );
        }
    }
}

fn snapshot_envelope_bytes() -> u64 {
    let section_count = 11_u64;
    let header_len = 76_u64;
    let directory_entry_len = 28_u64;
    header_len.saturating_add(section_count.saturating_mul(directory_entry_len))
}

fn percent_string(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0.00".to_string();
    }
    let basis_points = part.saturating_mul(10_000) / whole;
    format!("{}.{:02}", basis_points / 100, basis_points % 100)
}

fn build_engine(rules: usize) -> ZanzibarEngine {
    let service = ZanzibarEngine::builder()
        .evaluation_limits(evaluation_limits())
        .build();
    must(service.add_dsl(org_schema()), "failed to apply org schema");
    apply_relationships(&service, &generated_relationships(rules));
    service
}

fn evaluation_limits() -> EvaluationLimits {
    EvaluationLimits {
        max_depth: non_zero_u32(50),
        max_fanout_per_step: non_zero_u32(100_000),
        max_lookup_results: non_zero_u32(1_000),
    }
}

fn generated_relationships(rules: usize) -> Vec<Relationship> {
    let mut relationships = Vec::with_capacity(rules);
    relationships.extend(fixed_relationships());
    let generated = rules.saturating_sub(FIXED_RELATIONSHIP_COUNT);
    for index in 0..generated {
        relationships.push(generated_relationship(index));
    }
    relationships
}

fn apply_relationships(service: &ZanzibarEngine, relationships: &[Relationship]) {
    let mut batch = Vec::with_capacity(MUTATION_BATCH_LIMIT);
    for relationship in relationships {
        batch.push(RelationshipMutation::Touch(relationship.clone()));
        if batch.len() == MUTATION_BATCH_LIMIT {
            flush_relationships(service, &mut batch);
        }
    }
    flush_relationships(service, &mut batch);
}

fn flush_relationships(service: &ZanzibarEngine, batch: &mut Vec<RelationshipMutation>) {
    if batch.is_empty() {
        return;
    }
    let mutations = std::mem::take(batch);
    must(
        service.write_relationships_with_preconditions(mutations, []),
        "failed to apply relationship batch",
    );
}

fn org_schema() -> &'static str {
    r#"
    namespace group {
        relation member {}
    }

    namespace folder {
        relation viewer {}
        relation editor {}
        relation owner {}

        relation inherited_viewer {
            rewrite union(
                this,
                computed_userset(relation: "viewer"),
                computed_userset(relation: "editor"),
                computed_userset(relation: "owner")
            )
        }
    }

    namespace doc {
        relation parent {}
        relation viewer {}
        relation editor {}
        relation owner {}
        relation banned {}

        relation can_view {
            rewrite exclusion(
                union(
                    computed_userset(relation: "viewer"),
                    computed_userset(relation: "editor"),
                    computed_userset(relation: "owner"),
                    tuple_to_userset(tupleset: "parent", computed_userset: "inherited_viewer")
                ),
                computed_userset(relation: "banned")
            )
        }
    }
    "#
}

fn fixed_relationships() -> [Relationship; FIXED_RELATIONSHIP_COUNT] {
    [
        parse_relationship(format!("group:target_team#member@user:{TARGET_USER_ID}")),
        parse_relationship("folder:target_folder#viewer@group:target_team#member"),
        parse_relationship("doc:inherited_doc#parent@folder:target_folder#inherited_viewer"),
        parse_relationship("doc:direct_doc#viewer@group:target_team#member"),
        parse_relationship("doc:denied_doc#viewer@group:target_team#member"),
        parse_relationship(format!("doc:denied_doc#banned@user:{TARGET_USER_ID}")),
        parse_relationship(format!("group:edit_team#member@user:{EDITOR_USER_ID}")),
        parse_relationship("doc:edit_doc#editor@group:edit_team#member"),
        parse_relationship(format!("doc:owner_doc#owner@user:{OWNER_USER_ID}")),
    ]
}

fn generated_relationship(index: usize) -> Relationship {
    match index % 6 {
        0 => parse_relationship(format!(
            "doc:bulk_doc_{index:06}#viewer@group:target_team#member",
        )),
        1 => parse_relationship(format!(
            "doc:bulk_doc_{index:06}#parent@folder:bulk_folder_{:05}#inherited_viewer",
            index / 100,
        )),
        2 => parse_relationship(format!(
            "folder:bulk_folder_{:05}#viewer@group:bulk_team_{:05}#member",
            index / 100,
            index % 10_000,
        )),
        3 => parse_relationship(format!(
            "group:bulk_team_{:05}#member@user:bulk_user_{index:06}",
            index % 10_000,
        )),
        4 => parse_relationship(format!(
            "doc:bulk_doc_{index:06}#editor@group:bulk_team_{:05}#member",
            index % 10_000,
        )),
        _ => parse_relationship(format!(
            "doc:bulk_doc_{index:06}#banned@user:blocked_user_{index:06}",
        )),
    }
}

fn parse_relationship(value: impl AsRef<str>) -> Relationship {
    must(
        value.as_ref().parse(),
        "failed to parse benchmark relationship",
    )
}

fn unique_snapshot_path(label: &str, compression: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "simple_zanzibar_section_size_{label}_{compression}_{}_{}.szsnap",
        process::id(),
        unique_suffix(),
    ))
}

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_SUFFIX: AtomicU64 = AtomicU64::new(1);
    NEXT_SUFFIX.fetch_add(1, Ordering::Relaxed)
}

fn remove_file(path: &Path) {
    let _ = fs::remove_file(path);
}

fn benchmark_filters() -> Vec<String> {
    let mut filters = Vec::new();
    let mut skip_next = false;

    for argument in env::args().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if argument == "--bench" {
            continue;
        }
        if option_takes_value(argument.as_str()) {
            skip_next = !argument.contains('=');
            continue;
        }
        if argument.starts_with('-') {
            continue;
        }
        filters.push(argument);
    }
    filters
}

fn option_takes_value(argument: &str) -> bool {
    matches!(
        argument,
        "--sample-size"
            | "--warm-up-time"
            | "--measurement-time"
            | "--nresamples"
            | "--confidence-level"
            | "--significance-level"
            | "--noise-threshold"
            | "--profile-time"
            | "--plotting-backend"
            | "--save-baseline"
            | "--baseline"
            | "--load-baseline"
            | "--output-format"
    ) || argument.starts_with("--sample-size=")
        || argument.starts_with("--warm-up-time=")
        || argument.starts_with("--measurement-time=")
        || argument.starts_with("--nresamples=")
        || argument.starts_with("--confidence-level=")
        || argument.starts_with("--significance-level=")
        || argument.starts_with("--noise-threshold=")
        || argument.starts_with("--profile-time=")
        || argument.starts_with("--plotting-backend=")
        || argument.starts_with("--save-baseline=")
        || argument.starts_with("--baseline=")
        || argument.starts_with("--load-baseline=")
        || argument.starts_with("--output-format=")
}

fn should_benchmark(name: &str, filters: &[String]) -> bool {
    filters.is_empty() || filters.iter().any(|filter| name.contains(filter.as_str()))
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
}

fn must<T, E>(result: Result<T, E>, context: &str) -> T
where
    E: std::fmt::Display,
{
    match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{context}: {error}");
            process::abort();
        }
    }
}
