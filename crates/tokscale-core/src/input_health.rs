//! Scan-input health model.
//!
//! Third-party input data can be damaged in ways tokscale cannot fix. The
//! contract here is isolation without silence: a bad record is rejected and
//! counted, a broken input is skipped and reported, and neither may erase
//! data that other records or inputs produced. Only tokscale's own pipeline
//! invariants remain hard errors. See ADR 0001.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::clients::ClientId;
use crate::records::error::SessionParseError;
use crate::records::ParsedMessage;

/// Why a single record inside an otherwise readable input was rejected.
///
/// Reasons intentionally stay coarse: integrity projections need the kind and
/// frequency of damage, not a per-record forensic log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRejectionReason {
    MissingModel,
    UnverifiedUsageOwner,
    MissingTimestamp,
    MalformedRecord,
}

impl RecordRejectionReason {
    /// Stable serialization key. Cache shards and JSON output use this key,
    /// so it must never change for an existing variant.
    pub const fn key(self) -> &'static str {
        match self {
            Self::MissingModel => "missing-model",
            Self::UnverifiedUsageOwner => "unverified-usage-owner",
            Self::MissingTimestamp => "missing-timestamp",
            Self::MalformedRecord => "malformed-record",
        }
    }

    /// Human-readable label for a stored key. Unknown keys (written by a
    /// newer parser than this build) render as-is instead of being dropped.
    pub fn label_for_key(key: &str) -> &str {
        match key {
            "missing-model" => "Missing model",
            "unverified-usage-owner" => "Unverified usage owner",
            "missing-timestamp" => "Missing timestamp",
            "malformed-record" => "Malformed record",
            other => other,
        }
    }
}

/// Aggregated record rejections for one input unit.
///
/// Stores only per-reason counts. Raw paths, parser messages, and record
/// samples are intentionally discarded once the parser classifies damage.
/// Reasons are keyed by stable strings so shards written with reasons this
/// build does not know still round-trip losslessly through the cache.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
// Field attributes stay plain: shards serialize this with bincode, which is
// not self-describing, so `skip_serializing_if` would corrupt round-trips.
pub struct RejectionSummary {
    counts: BTreeMap<String, u64>,
}

impl RejectionSummary {
    pub fn record(&mut self, reason: RecordRejectionReason) {
        self.record_key(reason.key());
    }

    /// Record a rejection under a raw key. Used when rehydrating cached
    /// summaries whose keys may come from a newer parser.
    pub fn record_key(&mut self, key: &str) {
        *self.counts.entry(key.to_string()).or_insert(0) += 1;
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
    }

    pub fn entries(&self) -> impl Iterator<Item = RejectionEntry<'_>> {
        self.counts
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(key, count)| RejectionEntry {
                key,
                label: RecordRejectionReason::label_for_key(key),
                count: *count,
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectionEntry<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub count: u64,
}

/// A transient input-level failure used while a parser or adapter classifies
/// an interrupted scan. It is deliberately absent from `HealthSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFailure {
    pub operation: String,
    pub message: String,
}

impl InputFailure {
    pub fn new(operation: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            message: message.into(),
        }
    }
}

impl From<&SessionParseError> for InputFailure {
    fn from(error: &SessionParseError) -> Self {
        Self {
            operation: error.operation().to_string(),
            message: error.to_string(),
        }
    }
}

/// Availability of one input unit's data in the current generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum InputStatus {
    /// The input was scanned to the end. Its messages (possibly zero) and
    /// rejection counts are authoritative and cacheable.
    #[default]
    Complete,
    /// The scan was interrupted mid-input. Records confirmed before the
    /// interruption are kept; the number of affected records is unknown and
    /// the result must not be cached.
    Partial { failure: InputFailure },
    /// The input could not be read at all. No data was produced.
    Unavailable { failure: InputFailure },
}

impl InputStatus {
    pub fn failure(&self) -> Option<&InputFailure> {
        match self {
            Self::Complete => None,
            Self::Partial { failure } | Self::Unavailable { failure } => Some(failure),
        }
    }
}

/// Health of one input unit: client identity plus status plus rejection counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputHealth {
    pub client: ClientId,
    pub path: PathBuf,
    pub status: InputStatus,
    pub rejections: RejectionSummary,
}

impl InputHealth {
    pub fn is_clean(&self) -> bool {
        matches!(self.status, InputStatus::Complete) && self.rejections.is_empty()
    }
}

/// Aggregated health for one acquisition. Clean inputs are not retained;
/// their count is derivable from load metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataHealth {
    inputs: Vec<InputHealth>,
    examined_inputs: usize,
}

impl DataHealth {
    /// Retain an input's health only when there is something to report.
    pub fn record(&mut self, health: InputHealth) {
        self.examined_inputs += 1;
        if !health.is_clean() {
            self.inputs.push(health);
        }
    }

    pub fn merge(&mut self, other: DataHealth) {
        self.inputs.extend(other.inputs);
        self.examined_inputs += other.examined_inputs;
    }

    pub fn inputs(&self) -> &[InputHealth] {
        &self.inputs
    }

    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }

    pub fn rejected_records(&self) -> u64 {
        self.inputs
            .iter()
            .map(|input| input.rejections.total())
            .sum()
    }

    pub fn partial_inputs(&self) -> usize {
        self.inputs
            .iter()
            .filter(|input| matches!(input.status, InputStatus::Partial { .. }))
            .count()
    }

    pub fn failed_inputs(&self) -> usize {
        self.inputs
            .iter()
            .filter(|input| matches!(input.status, InputStatus::Unavailable { .. }))
            .count()
    }

    pub fn clean_inputs(&self) -> usize {
        self.examined_inputs.saturating_sub(self.inputs.len())
    }

    pub fn degraded_inputs(&self) -> usize {
        self.inputs
            .iter()
            .filter(|input| {
                matches!(input.status, InputStatus::Complete) && !input.rejections.is_empty()
            })
            .count()
    }

    /// Total issue count: every rejected record plus every partial or
    /// unavailable input counts as one issue.
    pub fn issue_count(&self) -> u64 {
        self.rejected_records() + (self.partial_inputs() + self.failed_inputs()) as u64
    }

    /// Serializable summary for generation state, exports, and JSON output.
    ///
    /// Detailed parser failures and representative input paths stop at this
    /// boundary. User-visible health contains only stable issue classes and
    /// aggregate counts.
    pub fn summarize(&self) -> HealthSummary {
        let mut grouped = BTreeMap::<(String, ClientId, String, String), HealthIssue>::new();
        for input in &self.inputs {
            for rejection in input.rejections.entries() {
                let entry = grouped
                    .entry((
                        "warning".to_string(),
                        input.client,
                        rejection.key.to_string(),
                        "record-skipped".to_string(),
                    ))
                    .or_insert_with(|| HealthIssue {
                        level: "warning".to_string(),
                        client: input.client,
                        issue: rejection.key.to_string(),
                        affected_inputs: 0,
                        rejected_records: Some(0),
                        handling: "record-skipped".to_string(),
                    });
                entry.affected_inputs += 1;
                let rejected_records = entry
                    .rejected_records
                    .as_mut()
                    .expect("record issue must carry a rejected-record count");
                *rejected_records = rejected_records
                    .checked_add(rejection.count)
                    .expect("aggregated rejected record count must fit in u64");
            }

            let input_issue = match input.status {
                InputStatus::Partial { .. } => Some(("partial-input", "confirmed-data-kept")),
                InputStatus::Unavailable { .. } => Some(("input-unavailable", "input-skipped")),
                InputStatus::Complete => None,
            };
            if let Some((issue, handling)) = input_issue {
                let entry = grouped
                    .entry((
                        "error".to_string(),
                        input.client,
                        issue.to_string(),
                        handling.to_string(),
                    ))
                    .or_insert_with(|| HealthIssue {
                        level: "error".to_string(),
                        client: input.client,
                        issue: issue.to_string(),
                        affected_inputs: 0,
                        rejected_records: None,
                        handling: handling.to_string(),
                    });
                entry.affected_inputs += 1;
            }
        }

        HealthSummary {
            complete: self.is_empty(),
            clean_inputs: self.clean_inputs(),
            degraded_inputs: self.degraded_inputs(),
            rejected_records: self.rejected_records(),
            partial_inputs: self.partial_inputs(),
            failed_inputs: self.failed_inputs(),
            issues: grouped.into_values().collect(),
        }
    }
}

/// Serializable health summary carried by a generation. `complete: true`
/// with no issues means every scanned input was healthy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthSummary {
    pub complete: bool,
    pub clean_inputs: usize,
    pub degraded_inputs: usize,
    pub rejected_records: u64,
    pub partial_inputs: usize,
    pub failed_inputs: usize,
    #[serde(default)]
    pub issues: Vec<HealthIssue>,
}

impl Default for HealthSummary {
    fn default() -> Self {
        Self {
            complete: true,
            clean_inputs: 0,
            degraded_inputs: 0,
            rejected_records: 0,
            partial_inputs: 0,
            failed_inputs: 0,
            issues: Vec::new(),
        }
    }
}

impl HealthSummary {
    /// Total issue count: every rejected record plus every partial or
    /// unavailable input counts as one issue.
    pub fn issue_count(&self) -> u64 {
        self.rejected_records + (self.partial_inputs + self.failed_inputs) as u64
    }

    /// Input-level failures may be transient even when the input inventory
    /// fingerprint is unchanged, so callers should retry those scans.
    pub fn requires_input_retry(&self) -> bool {
        self.partial_inputs > 0 || self.failed_inputs > 0
    }

    pub fn record_unavailable_input(&mut self, client: ClientId) {
        self.complete = false;
        if self.issues.iter().any(|issue| {
            issue.client == client
                && issue.issue == "input-unavailable"
                && issue.handling == "input-skipped"
        }) {
            return;
        }
        self.failed_inputs += 1;
        self.issues.push(HealthIssue {
            level: "error".to_string(),
            client,
            issue: "input-unavailable".to_string(),
            affected_inputs: 1,
            rejected_records: None,
            handling: "input-skipped".to_string(),
        });
    }
}

/// Stable, aggregate-only issue exposed by JSON and the TUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthIssue {
    pub level: String,
    pub client: ClientId,
    pub issue: String,
    /// Number of input units represented by this issue class.
    pub affected_inputs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_records: Option<u64>,
    pub handling: String,
}

/// What a session parser produced from scanning one input unit.
///
/// `Err(SessionParseError)` from a parser now means "the input could not be
/// read at all". Damage inside individual records must be recorded in
/// `rejections` instead of failing the scan, and damage that interrupts an
/// in-progress scan sets `interrupted` while keeping the records confirmed
/// so far.
#[derive(Debug, Default)]
pub struct ScannedInput {
    pub messages: Vec<ParsedMessage>,
    pub rejections: RejectionSummary,
    pub interrupted: Option<InputFailure>,
}

impl ScannedInput {
    pub fn complete(messages: Vec<ParsedMessage>) -> Self {
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

    fn health(status: InputStatus, rejections: RejectionSummary) -> InputHealth {
        InputHealth {
            client: ClientId::Zed,
            path: PathBuf::from("/tmp/threads.db"),
            status,
            rejections,
        }
    }

    #[test]
    fn rejection_summary_counts_by_reason() {
        let mut summary = RejectionSummary::default();
        summary.record(RecordRejectionReason::MissingModel);
        summary.record(RecordRejectionReason::MissingModel);
        summary.record(RecordRejectionReason::MalformedRecord);

        assert_eq!(summary.total(), 3);
        let entries: Vec<_> = summary.entries().collect();
        assert_eq!(entries.len(), 2);
        let missing_model = entries
            .iter()
            .find(|entry| entry.key == "missing-model")
            .unwrap();
        assert_eq!(missing_model.count, 2);
        assert_eq!(missing_model.label, "Missing model");
    }

    #[test]
    fn rejection_summary_round_trips_unknown_keys() {
        let mut summary = RejectionSummary::default();
        summary.record_key("future-reason");

        let serialized = serde_json::to_string(&summary).unwrap();
        let restored: RejectionSummary = serde_json::from_str(&serialized).unwrap();
        let entries: Vec<_> = restored.entries().collect();
        assert_eq!(entries[0].key, "future-reason");
        assert_eq!(entries[0].label, "future-reason");
        assert_eq!(entries[0].count, 1);
    }

    #[test]
    fn default_health_report_represents_a_complete_load() {
        let report = HealthSummary::default();

        assert!(report.complete);
        assert_eq!(report.clean_inputs, 0);
        assert_eq!(report.degraded_inputs, 0);
        assert_eq!(report.rejected_records, 0);
        assert_eq!(report.partial_inputs, 0);
        assert_eq!(report.failed_inputs, 0);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn empty_health_json_deserializes_as_a_complete_load() {
        let report: HealthSummary = serde_json::from_str("{}").unwrap();

        assert_eq!(report, HealthSummary::default());
    }

    #[test]
    fn complete_health_json_keeps_a_stable_empty_issues_array() {
        let value = serde_json::to_value(HealthSummary::default()).unwrap();

        assert_eq!(value["complete"], true);
        assert_eq!(value["cleanInputs"], 0);
        assert_eq!(value["degradedInputs"], 0);
        assert!(value.get("healthyInputs").is_none());
        assert_eq!(value["rejectedRecords"], 0);
        assert_eq!(value["partialInputs"], 0);
        assert_eq!(value["failedInputs"], 0);
        assert!(value.get("inputDataBytes").is_none());
        assert_eq!(value["issues"], serde_json::json!([]));
    }

    #[test]
    fn health_report_rejects_unknown_fields() {
        let error = serde_json::from_str::<HealthSummary>(r#"{"unexpectedField":1}"#)
            .expect_err("unknown health fields must not deserialize");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn health_report_issue_count_includes_records_and_input_failures() {
        let report = HealthSummary {
            complete: false,
            clean_inputs: 4,
            degraded_inputs: 1,
            rejected_records: 3,
            partial_inputs: 2,
            failed_inputs: 1,
            issues: Vec::new(),
        };

        assert_eq!(report.issue_count(), 6);
    }

    #[test]
    fn supplementary_unavailable_input_does_not_duplicate_existing_issue() {
        let mut report = HealthSummary::default();

        report.record_unavailable_input(ClientId::Claude);
        report.record_unavailable_input(ClientId::Claude);

        assert_eq!(report.failed_inputs, 1);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].affected_inputs, 1);
    }

    #[test]
    fn data_health_classifies_inputs_into_clean_degraded_partial_and_failed() {
        let mut data_health = DataHealth::default();
        data_health.record(health(InputStatus::Complete, RejectionSummary::default()));
        assert!(data_health.is_empty());

        let mut rejections = RejectionSummary::default();
        rejections.record(RecordRejectionReason::MissingModel);
        rejections.record(RecordRejectionReason::UnverifiedUsageOwner);
        data_health.record(health(InputStatus::Complete, rejections));
        data_health.record(health(
            InputStatus::Unavailable {
                failure: InputFailure::new("open SQLite input read-only", "corrupt header"),
            },
            RejectionSummary::default(),
        ));
        data_health.record(health(
            InputStatus::Partial {
                failure: InputFailure::new("scan rows", "disk I/O error mid-scan"),
            },
            RejectionSummary::default(),
        ));

        assert_eq!(data_health.rejected_records(), 2);
        assert_eq!(data_health.clean_inputs(), 1);
        assert_eq!(data_health.degraded_inputs(), 1);
        assert_eq!(data_health.failed_inputs(), 1);
        assert_eq!(data_health.partial_inputs(), 1);
        assert_eq!(data_health.issue_count(), 4);
    }

    #[test]
    fn merging_data_health_preserves_clean_and_degraded_input_counts() {
        let mut left = DataHealth::default();
        left.record(health(InputStatus::Complete, RejectionSummary::default()));
        let mut rejected = RejectionSummary::default();
        rejected.record(RecordRejectionReason::MissingModel);
        left.record(health(InputStatus::Complete, rejected));

        let mut right = DataHealth::default();
        right.record(health(InputStatus::Complete, RejectionSummary::default()));
        right.record(health(
            InputStatus::Unavailable {
                failure: InputFailure::new("open input", "missing"),
            },
            RejectionSummary::default(),
        ));

        left.merge(right);

        assert_eq!(left.clean_inputs(), 2);
        assert_eq!(left.degraded_inputs(), 1);
        assert_eq!(left.inputs().len(), 2);
        assert_eq!(left.failed_inputs(), 1);
        assert_eq!(left.rejected_records(), 1);
    }

    #[test]
    fn report_projection_aggregates_issue_classes_without_raw_details() {
        let mut data_health = DataHealth::default();
        for path in ["/sessions/first.jsonl", "/sessions/second.jsonl"] {
            let mut rejections = RejectionSummary::default();
            rejections.record(RecordRejectionReason::MalformedRecord);
            data_health.record(InputHealth {
                client: ClientId::Codex,
                path: PathBuf::from(path),
                status: InputStatus::Complete,
                rejections,
            });
        }

        for path in ["/sessions/third.jsonl", "/sessions/fourth.jsonl"] {
            data_health.record(InputHealth {
                client: ClientId::Codex,
                path: PathBuf::from(path),
                status: InputStatus::Unavailable {
                    failure: InputFailure::new("read JSONL", format!("failed: {path}")),
                },
                rejections: RejectionSummary::default(),
            });
        }

        let report = data_health.summarize();

        assert_eq!(report.clean_inputs, 0);
        assert_eq!(report.degraded_inputs, 2);
        assert_eq!(report.rejected_records, 2);
        assert_eq!(report.failed_inputs, 2);
        assert_eq!(report.issues.len(), 2);

        let records = report
            .issues
            .iter()
            .find(|issue| issue.issue == "malformed-record")
            .unwrap();
        assert_eq!(records.level, "warning");
        assert_eq!(records.client, ClientId::Codex);
        assert_eq!(records.affected_inputs, 2);
        assert_eq!(records.rejected_records, Some(2));
        assert_eq!(records.handling, "record-skipped");

        let failures = report
            .issues
            .iter()
            .find(|issue| issue.issue == "input-unavailable")
            .unwrap();
        assert_eq!(failures.level, "error");
        assert_eq!(failures.client, ClientId::Codex);
        assert_eq!(failures.affected_inputs, 2);
        assert_eq!(failures.rejected_records, None);
        assert_eq!(failures.handling, "input-skipped");
    }

    #[test]
    fn report_json_exposes_only_aggregated_health_issues() {
        let mut data_health = DataHealth::default();
        let mut rejections = RejectionSummary::default();
        rejections.record(RecordRejectionReason::MissingModel);
        data_health.record(InputHealth {
            client: ClientId::Zed,
            path: PathBuf::from("/private/zed/threads.db"),
            status: InputStatus::Complete,
            rejections,
        });
        data_health.record(InputHealth {
            client: ClientId::Kiro,
            path: PathBuf::from("/private/kiro/session.jsonl"),
            status: InputStatus::Unavailable {
                failure: InputFailure::new("decode private input", "raw parser failure"),
            },
            rejections: RejectionSummary::default(),
        });

        let value = serde_json::to_value(data_health.summarize()).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();

        assert!(value.get("inputs").is_none());
        assert_eq!(value["issues"].as_array().unwrap().len(), 2);
        assert!(!encoded.contains("/private/"));
        assert!(!encoded.contains("raw rejection detail"));
        assert!(!encoded.contains("decode private input"));
        assert!(!encoded.contains("raw parser failure"));
    }
}
