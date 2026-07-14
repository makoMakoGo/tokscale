//! Source health model.
//!
//! Third-party source data can be damaged in ways tokscale cannot fix. The
//! contract here is isolation without silence: a bad record is rejected and
//! counted, a broken source is skipped and reported, and neither may erase
//! data that other records or sources produced. Only tokscale's own pipeline
//! invariants remain hard errors. See ADR 0021.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::clients::ClientId;
use crate::sessions::error::SessionParseError;
use crate::UnifiedMessage;

/// Why a single record inside an otherwise readable source was rejected.
///
/// Reasons intentionally stay coarse: the Issues surface needs "what kind of
/// damage, how often, where", not a per-record forensic log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRejectionReason {
    MissingModel,
    MissingProvider,
    MissingTimestamp,
    MalformedRecord,
}

impl RecordRejectionReason {
    /// Stable serialization key. Cache shards and JSON output use this key,
    /// so it must never change for an existing variant.
    pub const fn key(self) -> &'static str {
        match self {
            Self::MissingModel => "missing-model",
            Self::MissingProvider => "missing-provider",
            Self::MissingTimestamp => "missing-timestamp",
            Self::MalformedRecord => "malformed-record",
        }
    }

    /// Human-readable label for a stored key. Unknown keys (written by a
    /// newer parser than this build) render as-is instead of being dropped.
    pub fn label_for_key(key: &str) -> &str {
        match key {
            "missing-model" => "Missing model",
            "missing-provider" => "Missing provider",
            "missing-timestamp" => "Missing timestamp",
            "malformed-record" => "Malformed record",
            other => other,
        }
    }
}

/// Aggregated record rejections for one source unit.
///
/// Stores only per-reason counts and one sample detail per reason. Reasons
/// are keyed by stable strings so shards written with reasons this build
/// does not know still round-trip losslessly through the cache.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// Field attributes stay plain: shards serialize this with bincode, which is
// not self-describing, so `skip_serializing_if` would corrupt round-trips.
pub struct RejectionSummary {
    counts: BTreeMap<String, u64>,
    samples: BTreeMap<String, String>,
}

impl RejectionSummary {
    pub fn record(&mut self, reason: RecordRejectionReason, sample: impl FnOnce() -> String) {
        self.record_key(reason.key(), sample);
    }

    /// Record a rejection under a raw key. Used when rehydrating cached
    /// summaries whose keys may come from a newer parser.
    pub fn record_key(&mut self, key: &str, sample: impl FnOnce() -> String) {
        *self.counts.entry(key.to_string()).or_insert(0) += 1;
        self.samples.entry(key.to_string()).or_insert_with(sample);
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub fn total(&self) -> u64 {
        self.counts.values().sum()
    }

    pub fn merge(&mut self, other: &RejectionSummary) {
        for (key, count) in &other.counts {
            *self.counts.entry(key.clone()).or_insert(0) += count;
        }
        for (key, sample) in &other.samples {
            self.samples
                .entry(key.clone())
                .or_insert_with(|| sample.clone());
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = RejectionEntry<'_>> {
        self.counts
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(key, count)| RejectionEntry {
                key,
                label: RecordRejectionReason::label_for_key(key),
                count: *count,
                sample: self.samples.get(key).map(String::as_str),
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectionEntry<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub count: u64,
    pub sample: Option<&'a str>,
}

/// A structured source-level failure: what operation failed and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFailure {
    pub operation: String,
    pub message: String,
}

impl SourceFailure {
    pub fn new(operation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            message: message.into(),
        }
    }
}

impl From<&SessionParseError> for SourceFailure {
    fn from(error: &SessionParseError) -> Self {
        Self {
            operation: error.operation().to_string(),
            message: error.to_string(),
        }
    }
}

/// Availability of one source unit's data in the current report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SourceStatus {
    /// The source was scanned to the end. Its messages (possibly zero) and
    /// rejection counts are authoritative and cacheable.
    #[default]
    Complete,
    /// The scan was interrupted mid-source. Records confirmed before the
    /// interruption are kept; the number of affected records is unknown and
    /// the result must not be cached.
    Partial { failure: SourceFailure },
    /// The source could not be read at all. No data was produced.
    Unavailable { failure: SourceFailure },
}

impl SourceStatus {
    pub fn failure(&self) -> Option<&SourceFailure> {
        match self {
            Self::Complete => None,
            Self::Partial { failure } | Self::Unavailable { failure } => Some(failure),
        }
    }
}

/// Health of one source unit: identity plus status plus rejection counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceHealth {
    pub client: ClientId,
    pub path: PathBuf,
    pub status: SourceStatus,
    pub rejections: RejectionSummary,
}

impl SourceHealth {
    pub fn is_healthy(&self) -> bool {
        matches!(self.status, SourceStatus::Complete) && self.rejections.is_empty()
    }
}

/// Aggregated health for one report load. Healthy sources are not retained;
/// their count is derivable from load metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataHealth {
    sources: Vec<SourceHealth>,
    examined_sources: usize,
}

impl DataHealth {
    /// Retain a source's health only when there is something to report.
    pub fn record(&mut self, health: SourceHealth) {
        self.examined_sources += 1;
        if !health.is_healthy() {
            self.sources.push(health);
        }
    }

    pub fn merge(&mut self, other: DataHealth) {
        self.sources.extend(other.sources);
        self.examined_sources += other.examined_sources;
    }

    pub fn sources(&self) -> &[SourceHealth] {
        &self.sources
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn rejected_records(&self) -> u64 {
        self.sources
            .iter()
            .map(|source| source.rejections.total())
            .sum()
    }

    pub fn partial_sources(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| matches!(source.status, SourceStatus::Partial { .. }))
            .count()
    }

    pub fn failed_sources(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| matches!(source.status, SourceStatus::Unavailable { .. }))
            .count()
    }

    pub fn healthy_sources(&self) -> usize {
        self.examined_sources.saturating_sub(self.sources.len())
    }

    /// Total issue count: every rejected record plus every partial or
    /// unavailable source counts as one issue.
    pub fn issue_count(&self) -> u64 {
        self.rejected_records() + (self.partial_sources() + self.failed_sources()) as u64
    }

    /// Serializable summary for report payloads and exports.
    pub fn to_report(&self) -> HealthReport {
        let mut grouped = BTreeMap::<(String, String, String), SourceHealthReport>::new();
        for source in &self.sources {
            let client = source.client.as_str().to_string();
            let sample_path = source.path.display().to_string();

            for rejection in source.rejections.entries() {
                let entry = grouped
                    .entry((
                        "record".to_string(),
                        client.clone(),
                        rejection.key.to_string(),
                    ))
                    .or_insert_with(|| SourceHealthReport {
                        client: client.clone(),
                        path: sample_path.clone(),
                        status: "complete".to_string(),
                        affected_sources: 0,
                        failure: None,
                        rejections: RejectionSummary::default(),
                    });
                entry.affected_sources += 1;
                *entry
                    .rejections
                    .counts
                    .entry(rejection.key.to_string())
                    .or_insert(0) += rejection.count;
                if let Some(sample) = rejection.sample {
                    entry
                        .rejections
                        .samples
                        .entry(rejection.key.to_string())
                        .or_insert_with(|| sample.to_string());
                }
            }

            if let Some(failure) = source.status.failure() {
                let status = match source.status {
                    SourceStatus::Partial { .. } => "partial",
                    SourceStatus::Unavailable { .. } => "unavailable",
                    SourceStatus::Complete => continue,
                };
                let entry = grouped
                    .entry((
                        "source".to_string(),
                        client.clone(),
                        format!("{status}:{}", failure.operation),
                    ))
                    .or_insert_with(|| SourceHealthReport {
                        client: client.clone(),
                        path: sample_path.clone(),
                        status: status.to_string(),
                        affected_sources: 0,
                        failure: Some(failure.clone()),
                        rejections: RejectionSummary::default(),
                    });
                entry.affected_sources += 1;
            }
        }

        HealthReport {
            complete: self.is_empty(),
            healthy_sources: self.healthy_sources(),
            rejected_records: self.rejected_records(),
            partial_sources: self.partial_sources(),
            failed_sources: self.failed_sources(),
            sources: grouped.into_values().collect(),
        }
    }
}

/// Serializable health summary carried by report payloads. `complete: true`
/// with no sources means every scanned source was healthy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HealthReport {
    pub complete: bool,
    pub healthy_sources: usize,
    pub rejected_records: u64,
    pub partial_sources: usize,
    pub failed_sources: usize,
    #[serde(default)]
    pub sources: Vec<SourceHealthReport>,
}

impl Default for HealthReport {
    fn default() -> Self {
        Self {
            complete: true,
            healthy_sources: 0,
            rejected_records: 0,
            partial_sources: 0,
            failed_sources: 0,
            sources: Vec::new(),
        }
    }
}

impl HealthReport {
    /// Total issue count: every rejected record plus every partial or
    /// unavailable source counts as one issue.
    pub fn issue_count(&self) -> u64 {
        self.rejected_records + (self.partial_sources + self.failed_sources) as u64
    }

    /// Source-level failures may be transient even when the source inventory
    /// fingerprint is unchanged, so callers should retry those scans.
    pub fn requires_source_retry(&self) -> bool {
        self.partial_sources > 0 || self.failed_sources > 0
    }

    pub fn record_unavailable_source(
        &mut self,
        client: &str,
        path: impl Into<String>,
        failure: SourceFailure,
    ) {
        let path = path.into();
        if self.sources.iter().any(|source| {
            source.client == client && source.status == "unavailable" && source.path == path
        }) {
            self.complete = false;
            return;
        }
        if let Some(existing) = self.sources.iter_mut().find(|source| {
            source.client == client
                && source.status == "unavailable"
                && source
                    .failure
                    .as_ref()
                    .is_some_and(|existing| existing.operation == failure.operation)
        }) {
            if existing.path != path {
                existing.affected_sources += 1;
                self.failed_sources += 1;
            }
            self.complete = false;
            return;
        }

        self.complete = false;
        self.failed_sources += 1;
        self.sources.push(SourceHealthReport {
            client: client.to_string(),
            path,
            status: "unavailable".to_string(),
            affected_sources: 1,
            failure: Some(failure),
            rejections: RejectionSummary::default(),
        });
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealthReport {
    pub client: String,
    /// One representative source path for this aggregated issue class.
    pub path: String,
    pub status: String,
    /// Number of source units represented by this issue class.
    pub affected_sources: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<SourceFailure>,
    #[serde(default, skip_serializing_if = "RejectionSummary::is_empty")]
    pub rejections: RejectionSummary,
}

/// What a session parser produced from scanning one source unit.
///
/// `Err(SessionParseError)` from a parser now means "the source could not be
/// read at all". Damage inside individual records must be recorded in
/// `rejections` instead of failing the scan, and damage that interrupts an
/// in-progress scan sets `interrupted` while keeping the records confirmed
/// so far.
#[derive(Debug, Default)]
pub struct ScannedSource {
    pub messages: Vec<UnifiedMessage>,
    pub rejections: RejectionSummary,
    pub interrupted: Option<SourceFailure>,
}

impl ScannedSource {
    pub fn complete(messages: Vec<UnifiedMessage>) -> Self {
        Self {
            messages,
            rejections: RejectionSummary::default(),
            interrupted: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(status: SourceStatus, rejections: RejectionSummary) -> SourceHealth {
        SourceHealth {
            client: ClientId::Zed,
            path: PathBuf::from("/tmp/threads.db"),
            status,
            rejections,
        }
    }

    #[test]
    fn rejection_summary_counts_and_keeps_first_sample() {
        let mut summary = RejectionSummary::default();
        summary.record(RecordRejectionReason::MissingModel, || "thread-1".into());
        summary.record(RecordRejectionReason::MissingModel, || "thread-2".into());
        summary.record(RecordRejectionReason::MalformedRecord, || "row 9".into());

        assert_eq!(summary.total(), 3);
        let entries: Vec<_> = summary.entries().collect();
        assert_eq!(entries.len(), 2);
        let missing_model = entries
            .iter()
            .find(|entry| entry.key == "missing-model")
            .unwrap();
        assert_eq!(missing_model.count, 2);
        assert_eq!(missing_model.label, "Missing model");
        assert_eq!(missing_model.sample, Some("thread-1"));
    }

    #[test]
    fn rejection_summary_round_trips_unknown_keys() {
        let mut summary = RejectionSummary::default();
        summary.record_key("future-reason", || "sample".into());

        let serialized = serde_json::to_string(&summary).unwrap();
        let restored: RejectionSummary = serde_json::from_str(&serialized).unwrap();
        let entries: Vec<_> = restored.entries().collect();
        assert_eq!(entries[0].key, "future-reason");
        assert_eq!(entries[0].label, "future-reason");
        assert_eq!(entries[0].count, 1);
    }

    #[test]
    fn default_health_report_represents_a_complete_load() {
        let report = HealthReport::default();

        assert!(report.complete);
        assert_eq!(report.healthy_sources, 0);
        assert_eq!(report.rejected_records, 0);
        assert_eq!(report.partial_sources, 0);
        assert_eq!(report.failed_sources, 0);
        assert!(report.sources.is_empty());
    }

    #[test]
    fn empty_health_json_deserializes_as_a_complete_load() {
        let report: HealthReport = serde_json::from_str("{}").unwrap();

        assert_eq!(report, HealthReport::default());
    }

    #[test]
    fn complete_health_json_keeps_a_stable_empty_sources_array() {
        let value = serde_json::to_value(HealthReport::default()).unwrap();

        assert_eq!(value["complete"], true);
        assert_eq!(value["healthySources"], 0);
        assert_eq!(value["rejectedRecords"], 0);
        assert_eq!(value["partialSources"], 0);
        assert_eq!(value["failedSources"], 0);
        assert_eq!(value["sources"], serde_json::json!([]));
    }

    #[test]
    fn health_report_issue_count_includes_records_and_source_failures() {
        let report = HealthReport {
            complete: false,
            healthy_sources: 4,
            rejected_records: 3,
            partial_sources: 2,
            failed_sources: 1,
            sources: Vec::new(),
        };

        assert_eq!(report.issue_count(), 6);
    }

    #[test]
    fn data_health_drops_healthy_sources_and_counts_issues() {
        let mut data_health = DataHealth::default();
        data_health.record(health(SourceStatus::Complete, RejectionSummary::default()));
        assert!(data_health.is_empty());

        let mut rejections = RejectionSummary::default();
        rejections.record(RecordRejectionReason::MissingModel, || "t".into());
        rejections.record(RecordRejectionReason::MissingProvider, || "u".into());
        data_health.record(health(SourceStatus::Complete, rejections));
        data_health.record(health(
            SourceStatus::Unavailable {
                failure: SourceFailure::new("open SQLite source read-only", "corrupt header"),
            },
            RejectionSummary::default(),
        ));
        data_health.record(health(
            SourceStatus::Partial {
                failure: SourceFailure::new("scan rows", "disk I/O error mid-scan"),
            },
            RejectionSummary::default(),
        ));

        assert_eq!(data_health.rejected_records(), 2);
        assert_eq!(data_health.healthy_sources(), 1);
        assert_eq!(data_health.failed_sources(), 1);
        assert_eq!(data_health.partial_sources(), 1);
        assert_eq!(data_health.issue_count(), 4);
    }

    #[test]
    fn merging_data_health_preserves_examined_and_healthy_source_counts() {
        let mut left = DataHealth::default();
        left.record(health(SourceStatus::Complete, RejectionSummary::default()));
        let mut rejected = RejectionSummary::default();
        rejected.record(RecordRejectionReason::MissingModel, || "bad".into());
        left.record(health(SourceStatus::Complete, rejected));

        let mut right = DataHealth::default();
        right.record(health(SourceStatus::Complete, RejectionSummary::default()));
        right.record(health(
            SourceStatus::Unavailable {
                failure: SourceFailure::new("open source", "missing"),
            },
            RejectionSummary::default(),
        ));

        left.merge(right);

        assert_eq!(left.healthy_sources(), 2);
        assert_eq!(left.sources().len(), 2);
        assert_eq!(left.failed_sources(), 1);
        assert_eq!(left.rejected_records(), 1);
    }

    #[test]
    fn report_projection_aggregates_issue_classes_with_one_sample() {
        let mut data_health = DataHealth::default();
        for (path, sample) in [
            ("/sessions/first.jsonl", "first"),
            ("/sessions/second.jsonl", "second"),
        ] {
            let mut rejections = RejectionSummary::default();
            rejections.record(RecordRejectionReason::MalformedRecord, || {
                sample.to_string()
            });
            data_health.record(SourceHealth {
                client: ClientId::Codex,
                path: PathBuf::from(path),
                status: SourceStatus::Complete,
                rejections,
            });
        }

        for path in ["/sessions/third.jsonl", "/sessions/fourth.jsonl"] {
            data_health.record(SourceHealth {
                client: ClientId::Codex,
                path: PathBuf::from(path),
                status: SourceStatus::Unavailable {
                    failure: SourceFailure::new("read JSONL", format!("failed: {path}")),
                },
                rejections: RejectionSummary::default(),
            });
        }

        let report = data_health.to_report();

        assert_eq!(report.rejected_records, 2);
        assert_eq!(report.failed_sources, 2);
        assert_eq!(report.sources.len(), 2);

        let records = report
            .sources
            .iter()
            .find(|source| !source.rejections.is_empty())
            .unwrap();
        assert_eq!(records.affected_sources, 2);
        assert_eq!(records.path, "/sessions/first.jsonl");
        let rejection = records.rejections.entries().next().unwrap();
        assert_eq!(rejection.count, 2);
        assert_eq!(rejection.sample, Some("first"));

        let failures = report
            .sources
            .iter()
            .find(|source| source.failure.is_some())
            .unwrap();
        assert_eq!(failures.affected_sources, 2);
        assert_eq!(failures.path, "/sessions/third.jsonl");
        assert!(failures
            .failure
            .as_ref()
            .unwrap()
            .message
            .contains("third.jsonl"));
    }
}
