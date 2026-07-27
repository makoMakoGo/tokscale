//! Scan-input health model.
//!
//! Third-party input data can be damaged in ways tokenx cannot fix. The
//! contract here is isolation without silence: a bad record is rejected and
//! counted, a broken input is skipped and reported, and neither may erase
//! data that other records or inputs produced. Only tokenx's own pipeline
//! invariants remain hard errors. See ADR 0001.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::de::Visitor;
use serde::{Deserialize, Serialize};

use crate::clients::ClientId;
use crate::records::error::SessionParseError;
use crate::records::UsageRecord;

/// Non-authoritative cache failures observed while acquiring one input.
///
/// These diagnostics never change the authority of successfully parsed input
/// records. They exist so disposable cache failures are visible without
/// turning a valid scan into a failed acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDiagnosticKind {
    CacheUnavailable,
    CacheReadFailed,
    CacheWriteFailed,
}

impl InputDiagnosticKind {
    const fn issue(self) -> HealthIssueKind {
        match self {
            Self::CacheUnavailable => HealthIssueKind::InputCacheUnavailable,
            Self::CacheReadFailed => HealthIssueKind::InputCacheReadFailed,
            Self::CacheWriteFailed => HealthIssueKind::InputCacheWriteFailed,
        }
    }

    const fn handling(self) -> HealthHandling {
        match self {
            Self::CacheUnavailable => HealthHandling::CacheBypassed,
            Self::CacheReadFailed => HealthHandling::InputReparsed,
            Self::CacheWriteFailed => HealthHandling::AuthoritativeDataKept,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputDiagnostic {
    client: Option<ClientId>,
    path: PathBuf,
    kind: InputDiagnosticKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthLevel {
    Warning,
    Error,
}

impl HealthLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for HealthLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl PartialEq<&str> for HealthLevel {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthIssueKind {
    RecordRejection(String),
    PartialInput,
    InputUnavailable,
    InputCacheUnavailable,
    InputCacheReadFailed,
    InputCacheWriteFailed,
}

impl HealthIssueKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::RecordRejection(key) => key,
            Self::PartialInput => "partial-input",
            Self::InputUnavailable => "input-unavailable",
            Self::InputCacheUnavailable => "input-cache-unavailable",
            Self::InputCacheReadFailed => "input-cache-read-failed",
            Self::InputCacheWriteFailed => "input-cache-write-failed",
        }
    }

    fn from_key(key: String) -> Self {
        match key.as_str() {
            "partial-input" => Self::PartialInput,
            "input-unavailable" => Self::InputUnavailable,
            "input-cache-unavailable" => Self::InputCacheUnavailable,
            "input-cache-read-failed" => Self::InputCacheReadFailed,
            "input-cache-write-failed" => Self::InputCacheWriteFailed,
            _ => Self::RecordRejection(key),
        }
    }

    pub const fn is_input_retry(&self) -> bool {
        matches!(self, Self::PartialInput | Self::InputUnavailable)
    }

    pub const fn is_cache_diagnostic(&self) -> bool {
        matches!(
            self,
            Self::InputCacheUnavailable | Self::InputCacheReadFailed | Self::InputCacheWriteFailed
        )
    }
}

impl Serialize for HealthIssueKind {
    fn serialize<Serializer>(
        &self,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HealthIssueKind {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        struct KindVisitor;
        impl Visitor<'_> for KindVisitor {
            type Value = HealthIssueKind;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a health issue key")
            }

            fn visit_str<Error>(self, value: &str) -> Result<Self::Value, Error>
            where
                Error: serde::de::Error,
            {
                Ok(HealthIssueKind::from_key(value.to_string()))
            }

            fn visit_string<Error>(self, value: String) -> Result<Self::Value, Error>
            where
                Error: serde::de::Error,
            {
                Ok(HealthIssueKind::from_key(value))
            }
        }
        deserializer.deserialize_string(KindVisitor)
    }
}

impl PartialEq<&str> for HealthIssueKind {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl fmt::Display for HealthIssueKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthHandling {
    RecordSkipped,
    ConfirmedDataKept,
    InputSkipped,
    CacheBypassed,
    InputReparsed,
    AuthoritativeDataKept,
    Other(String),
}

impl HealthHandling {
    pub fn as_str(&self) -> &str {
        match self {
            Self::RecordSkipped => "record-skipped",
            Self::ConfirmedDataKept => "confirmed-data-kept",
            Self::InputSkipped => "input-skipped",
            Self::CacheBypassed => "cache-bypassed",
            Self::InputReparsed => "input-reparsed",
            Self::AuthoritativeDataKept => "authoritative-data-kept",
            Self::Other(value) => value,
        }
    }

    fn from_key(key: String) -> Self {
        match key.as_str() {
            "record-skipped" => Self::RecordSkipped,
            "confirmed-data-kept" => Self::ConfirmedDataKept,
            "input-skipped" => Self::InputSkipped,
            "cache-bypassed" => Self::CacheBypassed,
            "input-reparsed" => Self::InputReparsed,
            "authoritative-data-kept" => Self::AuthoritativeDataKept,
            _ => Self::Other(key),
        }
    }
}

impl Serialize for HealthHandling {
    fn serialize<Serializer>(
        &self,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HealthHandling {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        struct HandlingVisitor;
        impl Visitor<'_> for HandlingVisitor {
            type Value = HealthHandling;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a health handling key")
            }

            fn visit_str<Error>(self, value: &str) -> Result<Self::Value, Error>
            where
                Error: serde::de::Error,
            {
                Ok(HealthHandling::from_key(value.to_string()))
            }

            fn visit_string<Error>(self, value: String) -> Result<Self::Value, Error>
            where
                Error: serde::de::Error,
            {
                Ok(HealthHandling::from_key(value))
            }
        }
        deserializer.deserialize_string(HandlingVisitor)
    }
}

impl PartialEq<&str> for HealthHandling {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl fmt::Display for HealthHandling {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a single record inside an otherwise readable input was rejected.
///
/// Reasons intentionally stay coarse: integrity projections need the kind and
/// frequency of damage, not a per-record forensic log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRejectionReason {
    MissingModel,
    MissingSession,
    UnverifiedUsageOwner,
    MissingTimestamp,
    InvalidUsageRecord,
    PricingComputationFailed,
    AggregationOverflow,
    MalformedRecord,
}

impl RecordRejectionReason {
    /// Stable serialization key. Cache shards and JSON output use this key,
    /// so it must never change for an existing variant.
    pub const fn key(self) -> &'static str {
        match self {
            Self::MissingModel => "missing-model",
            Self::MissingSession => "missing-session",
            Self::UnverifiedUsageOwner => "unverified-usage-owner",
            Self::MissingTimestamp => "missing-timestamp",
            Self::InvalidUsageRecord => "invalid-usage-record",
            Self::PricingComputationFailed => "pricing-computation-failed",
            Self::AggregationOverflow => "aggregation-overflow",
            Self::MalformedRecord => "malformed-record",
        }
    }

    /// Human-readable label for a stored key. Unknown keys (written by a
    /// newer parser than this build) render as-is instead of being dropped.
    pub fn label_for_key(key: &str) -> &str {
        match key {
            "missing-model" => "Missing model",
            "missing-session" => "Missing session",
            "unverified-usage-owner" => "Unverified usage owner",
            "missing-timestamp" => "Missing timestamp",
            "invalid-usage-record" => "Invalid usage record",
            "pricing-computation-failed" => "Pricing computation failed",
            "aggregation-overflow" => "Aggregation overflow",
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
///
/// Health is observational metadata, not usage authority. Its counters
/// saturate at their integer maximum so damaged or adversarial diagnostics
/// cannot abort an otherwise valid local-data generation.
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
        let count = self.counts.entry(key.to_string()).or_insert(0);
        *count = count.saturating_add(1);
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub fn total(&self) -> u64 {
        self.counts
            .values()
            .copied()
            .fold(0_u64, u64::saturating_add)
    }

    pub fn merge(&mut self, other: &RejectionSummary) {
        for (key, count) in &other.counts {
            let target = self.counts.entry(key.clone()).or_insert(0);
            *target = target.saturating_add(*count);
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

/// A transient input-level failure used while a parser or driver classifies
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
    diagnostics: Vec<InputDiagnostic>,
    examined_inputs: usize,
}

impl DataHealth {
    /// Retain an input's health only when there is something to report.
    pub fn record(&mut self, health: InputHealth) {
        self.examined_inputs = self.examined_inputs.saturating_add(1);
        if !health.is_clean() {
            if let Some(failure) = health.status.failure() {
                tracing::warn!(
                    client = health.client.as_str(),
                    path = %health.path.display(),
                    operation = %failure.operation,
                    error = %failure.message,
                    "input acquisition was degraded"
                );
            }
            self.inputs.push(health);
        }
    }

    pub fn merge(&mut self, other: DataHealth) {
        self.inputs.extend(other.inputs);
        self.diagnostics.extend(other.diagnostics);
        self.examined_inputs = self.examined_inputs.saturating_add(other.examined_inputs);
    }

    pub(crate) fn record_diagnostic(
        &mut self,
        client: ClientId,
        path: PathBuf,
        kind: InputDiagnosticKind,
        failure: InputFailure,
    ) {
        tracing::warn!(
            client = client.as_str(),
            path = %path.display(),
            operation = %failure.operation,
            error = %failure.message,
            diagnostic = kind.issue().as_str(),
            "input cache diagnostic"
        );
        self.diagnostics.push(InputDiagnostic {
            client: Some(client),
            path,
            kind,
        });
    }

    pub(crate) fn record_global_diagnostic(
        &mut self,
        path: PathBuf,
        kind: InputDiagnosticKind,
        failure: InputFailure,
    ) {
        tracing::warn!(
            path = %path.display(),
            operation = %failure.operation,
            error = %failure.message,
            diagnostic = kind.issue().as_str(),
            "global input cache diagnostic"
        );
        self.diagnostics.push(InputDiagnostic {
            client: None,
            path,
            kind,
        });
    }

    pub fn inputs(&self) -> &[InputHealth] {
        &self.inputs
    }

    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.diagnostics.is_empty()
    }

    pub fn rejected_records(&self) -> u64 {
        self.inputs
            .iter()
            .map(|input| input.rejections.total())
            .fold(0_u64, u64::saturating_add)
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
        let reported_inputs = self
            .inputs
            .iter()
            .map(|input| (input.client, input.path.as_path()))
            .collect::<std::collections::HashSet<_>>();
        let diagnostic_only_inputs = self
            .diagnostics
            .iter()
            .filter_map(|diagnostic| {
                diagnostic
                    .client
                    .map(|client| (client, diagnostic.path.as_path()))
            })
            .filter(|identity| !reported_inputs.contains(identity))
            .collect::<std::collections::HashSet<_>>();
        self.examined_inputs
            .saturating_sub(self.inputs.len() + diagnostic_only_inputs.len())
    }

    pub fn degraded_inputs(&self) -> usize {
        let rejected_inputs = self
            .inputs
            .iter()
            .filter(|input| {
                matches!(input.status, InputStatus::Complete) && !input.rejections.is_empty()
            })
            .count();
        let reported_inputs = self
            .inputs
            .iter()
            .map(|input| (input.client, input.path.as_path()))
            .collect::<std::collections::HashSet<_>>();
        let diagnostic_only_inputs = self
            .diagnostics
            .iter()
            .filter_map(|diagnostic| {
                diagnostic
                    .client
                    .map(|client| (client, diagnostic.path.as_path()))
            })
            .filter(|identity| !reported_inputs.contains(identity))
            .collect::<std::collections::HashSet<_>>();
        rejected_inputs.saturating_add(diagnostic_only_inputs.len())
    }

    /// Total issue count: every rejected record plus every partial or
    /// unavailable input counts as one issue.
    pub fn issue_count(&self) -> u64 {
        self.rejected_records()
            .saturating_add(
                u64::try_from(self.partial_inputs().saturating_add(self.failed_inputs()))
                    .unwrap_or(u64::MAX),
            )
            .saturating_add(u64::try_from(self.diagnostics.len()).unwrap_or(u64::MAX))
    }

    /// Serializable summary for generation state, exports, and JSON output.
    ///
    /// Detailed parser failures and representative input paths stop at this
    /// boundary. User-visible health contains only stable issue classes and
    /// aggregate counts.
    pub fn summarize(&self) -> HealthSummary {
        let mut grouped = BTreeMap::<
            (
                HealthLevel,
                Option<ClientId>,
                HealthIssueKind,
                HealthHandling,
            ),
            HealthIssue,
        >::new();
        for input in &self.inputs {
            for rejection in input.rejections.entries() {
                let issue = HealthIssueKind::RecordRejection(rejection.key.to_string());
                let entry = grouped
                    .entry((
                        HealthLevel::Warning,
                        Some(input.client),
                        issue.clone(),
                        HealthHandling::RecordSkipped,
                    ))
                    .or_insert_with(|| HealthIssue {
                        level: HealthLevel::Warning,
                        client: Some(input.client),
                        issue,
                        affected_inputs: 0,
                        rejected_records: Some(0),
                        handling: HealthHandling::RecordSkipped,
                    });
                entry.affected_inputs = entry.affected_inputs.saturating_add(1);
                let rejected_records = entry.rejected_records.get_or_insert(0);
                *rejected_records = rejected_records.saturating_add(rejection.count);
            }

            let input_issue = match input.status {
                InputStatus::Partial { .. } => Some((
                    HealthIssueKind::PartialInput,
                    HealthHandling::ConfirmedDataKept,
                )),
                InputStatus::Unavailable { .. } => Some((
                    HealthIssueKind::InputUnavailable,
                    HealthHandling::InputSkipped,
                )),
                InputStatus::Complete => None,
            };
            if let Some((issue, handling)) = input_issue {
                let entry = grouped
                    .entry((
                        HealthLevel::Error,
                        Some(input.client),
                        issue.clone(),
                        handling.clone(),
                    ))
                    .or_insert_with(|| HealthIssue {
                        level: HealthLevel::Error,
                        client: Some(input.client),
                        issue,
                        affected_inputs: 0,
                        rejected_records: None,
                        handling,
                    });
                entry.affected_inputs = entry.affected_inputs.saturating_add(1);
            }
        }
        for diagnostic in &self.diagnostics {
            let issue = diagnostic.kind.issue();
            let handling = diagnostic.kind.handling();
            let entry = grouped
                .entry((
                    HealthLevel::Warning,
                    diagnostic.client,
                    issue.clone(),
                    handling.clone(),
                ))
                .or_insert_with(|| HealthIssue {
                    level: HealthLevel::Warning,
                    client: diagnostic.client,
                    issue,
                    affected_inputs: 0,
                    rejected_records: None,
                    handling,
                });
            entry.affected_inputs = entry.affected_inputs.saturating_add(1);
        }

        HealthSummary {
            clean_inputs: self.clean_inputs(),
            degraded_inputs: self.degraded_inputs(),
            issues: grouped.into_values().collect(),
        }
    }
}

/// Serializable health summary carried by a generation.
///
/// Issue classes are the sole authority for completeness and issue counts.
/// The JSON wire keeps convenient derived counters, but deserialization
/// verifies them before discarding the redundant copies.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HealthSummary {
    pub clean_inputs: usize,
    pub degraded_inputs: usize,
    pub issues: Vec<HealthIssue>,
}

impl HealthSummary {
    pub fn complete(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn rejected_records(&self) -> u64 {
        self.issues
            .iter()
            .filter_map(|issue| issue.rejected_records)
            .fold(0_u64, u64::saturating_add)
    }

    pub fn partial_inputs(&self) -> usize {
        self.affected_inputs_for(HealthIssueKind::PartialInput)
    }

    pub fn failed_inputs(&self) -> usize {
        self.affected_inputs_for(HealthIssueKind::InputUnavailable)
    }

    /// Total issue count: every rejected record plus every partial or
    /// unavailable input counts as one issue.
    pub fn issue_count(&self) -> u64 {
        let input_cache_issues = self
            .issues
            .iter()
            .filter(|issue| issue.issue.is_cache_diagnostic())
            .map(|issue| issue.affected_inputs)
            .fold(0_u64, u64::saturating_add);
        self.rejected_records()
            .saturating_add(
                u64::try_from(self.partial_inputs().saturating_add(self.failed_inputs()))
                    .unwrap_or(u64::MAX),
            )
            .saturating_add(input_cache_issues)
    }

    /// Input-level failures may be transient even when the input inventory
    /// fingerprint is unchanged, so callers should retry those scans.
    pub fn requires_input_retry(&self) -> bool {
        self.partial_inputs() > 0 || self.failed_inputs() > 0
    }

    pub fn validate(&self) -> Result<(), HealthSummaryValidationError> {
        let mut identities = std::collections::BTreeSet::new();
        for issue in &self.issues {
            issue.validate()?;
            if !identities.insert((issue.client, issue.issue.clone())) {
                return Err(HealthSummaryValidationError::DuplicateIssue {
                    client: issue.client,
                    issue: issue.issue.to_string(),
                });
            }
        }
        if self.degraded_inputs > 0
            && !self
                .issues
                .iter()
                .any(|issue| issue.level == HealthLevel::Warning)
        {
            return Err(HealthSummaryValidationError::InvalidDegradedInputCount);
        }
        Ok(())
    }

    fn affected_inputs_for(&self, kind: HealthIssueKind) -> usize {
        self.issues
            .iter()
            .filter(|issue| issue.issue == kind)
            .try_fold(0_usize, |total, issue| {
                usize::try_from(issue.affected_inputs)
                    .ok()
                    .and_then(|count| total.checked_add(count))
            })
            .unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HealthSummaryValidationError {
    #[error("health issue `{issue}` must affect at least one input")]
    EmptyIssue { issue: String },
    #[error("health issue `{issue}` has invalid level `{actual}`; expected `{expected}`")]
    InvalidLevel {
        issue: String,
        expected: HealthLevel,
        actual: HealthLevel,
    },
    #[error("health issue `{issue}` has invalid handling `{actual}`; expected `{expected}`")]
    InvalidHandling {
        issue: String,
        expected: HealthHandling,
        actual: HealthHandling,
    },
    #[error("health issue `{issue}` has an invalid rejected-record count")]
    InvalidRejectedRecords { issue: String },
    #[error("health summary repeats issue `{issue}` for client {client:?}")]
    DuplicateIssue {
        client: Option<ClientId>,
        issue: String,
    },
    #[error("degraded input count requires at least one warning issue")]
    InvalidDegradedInputCount,
    #[error("health summary `{field}` does not match its issues")]
    DerivedCountMismatch { field: &'static str },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HealthSummaryWire {
    complete: bool,
    clean_inputs: usize,
    degraded_inputs: usize,
    rejected_records: u64,
    partial_inputs: usize,
    failed_inputs: usize,
    issues: Vec<HealthIssue>,
}

impl Serialize for HealthSummary {
    fn serialize<Serializer>(
        &self,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        HealthSummaryWire {
            complete: self.complete(),
            clean_inputs: self.clean_inputs,
            degraded_inputs: self.degraded_inputs,
            rejected_records: self.rejected_records(),
            partial_inputs: self.partial_inputs(),
            failed_inputs: self.failed_inputs(),
            issues: self.issues.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HealthSummary {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let wire = HealthSummaryWire::deserialize(deserializer)?;
        let summary = Self {
            clean_inputs: wire.clean_inputs,
            degraded_inputs: wire.degraded_inputs,
            issues: wire.issues,
        };
        summary.validate().map_err(serde::de::Error::custom)?;
        for (field, actual, expected) in [
            (
                "complete",
                u64::from(wire.complete),
                u64::from(summary.complete()),
            ),
            (
                "rejectedRecords",
                wire.rejected_records,
                summary.rejected_records(),
            ),
            (
                "partialInputs",
                u64::try_from(wire.partial_inputs).unwrap_or(u64::MAX),
                u64::try_from(summary.partial_inputs()).unwrap_or(u64::MAX),
            ),
            (
                "failedInputs",
                u64::try_from(wire.failed_inputs).unwrap_or(u64::MAX),
                u64::try_from(summary.failed_inputs()).unwrap_or(u64::MAX),
            ),
        ] {
            if actual != expected {
                return Err(serde::de::Error::custom(
                    HealthSummaryValidationError::DerivedCountMismatch { field },
                ));
            }
        }
        Ok(summary)
    }
}

/// Stable, aggregate-only issue exposed by JSON and the TUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthIssue {
    pub level: HealthLevel,
    #[serde(default)]
    pub client: Option<ClientId>,
    pub issue: HealthIssueKind,
    /// Number of input units represented by this issue class.
    pub affected_inputs: u64,
    #[serde(default)]
    pub rejected_records: Option<u64>,
    pub handling: HealthHandling,
}

impl HealthIssue {
    fn validate(&self) -> Result<(), HealthSummaryValidationError> {
        let issue = self.issue.to_string();
        if self.affected_inputs == 0 {
            return Err(HealthSummaryValidationError::EmptyIssue { issue });
        }
        let (expected_level, expected_handling, requires_rejected_records) = match self.issue {
            HealthIssueKind::RecordRejection(_) => {
                (HealthLevel::Warning, HealthHandling::RecordSkipped, true)
            }
            HealthIssueKind::PartialInput => {
                (HealthLevel::Error, HealthHandling::ConfirmedDataKept, false)
            }
            HealthIssueKind::InputUnavailable => {
                (HealthLevel::Error, HealthHandling::InputSkipped, false)
            }
            HealthIssueKind::InputCacheUnavailable => {
                (HealthLevel::Warning, HealthHandling::CacheBypassed, false)
            }
            HealthIssueKind::InputCacheReadFailed => {
                (HealthLevel::Warning, HealthHandling::InputReparsed, false)
            }
            HealthIssueKind::InputCacheWriteFailed => (
                HealthLevel::Warning,
                HealthHandling::AuthoritativeDataKept,
                false,
            ),
        };
        if self.level != expected_level {
            return Err(HealthSummaryValidationError::InvalidLevel {
                issue,
                expected: expected_level,
                actual: self.level,
            });
        }
        if self.handling != expected_handling {
            return Err(HealthSummaryValidationError::InvalidHandling {
                issue,
                expected: expected_handling,
                actual: self.handling.clone(),
            });
        }
        let invalid_rejected_records = if requires_rejected_records {
            !self.rejected_records.is_some_and(|count| count > 0)
        } else {
            self.rejected_records.is_some()
        };
        if invalid_rejected_records {
            return Err(HealthSummaryValidationError::InvalidRejectedRecords { issue });
        }
        Ok(())
    }
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
    pub messages: Vec<UsageRecord>,
    pub rejections: RejectionSummary,
    pub interrupted: Option<InputFailure>,
}

impl ScannedInput {
    pub fn complete(messages: Vec<UsageRecord>) -> Self {
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
    fn observational_health_counts_saturate_instead_of_panicking() {
        let mut summary = RejectionSummary::default();
        summary.counts.insert("missing-model".to_string(), u64::MAX);
        summary.record(RecordRejectionReason::MissingModel);
        summary.counts.insert("malformed-record".to_string(), 1);
        assert_eq!(summary.total(), u64::MAX);

        let mut other = RejectionSummary::default();
        other
            .counts
            .insert("malformed-record".to_string(), u64::MAX);
        summary.merge(&other);
        assert_eq!(summary.total(), u64::MAX);
        assert_eq!(
            summary.counts.get("malformed-record").copied(),
            Some(u64::MAX)
        );
    }

    #[test]
    fn default_health_report_represents_a_complete_load() {
        let report = HealthSummary::default();

        assert!(report.complete());
        assert_eq!(report.clean_inputs, 0);
        assert_eq!(report.degraded_inputs, 0);
        assert_eq!(report.rejected_records(), 0);
        assert_eq!(report.partial_inputs(), 0);
        assert_eq!(report.failed_inputs(), 0);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn incomplete_health_json_is_rejected() {
        let error = serde_json::from_str::<HealthSummary>("{}").unwrap_err();

        assert!(error.to_string().contains("missing field"));
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
            clean_inputs: 4,
            degraded_inputs: 1,
            issues: vec![
                HealthIssue {
                    level: HealthLevel::Warning,
                    client: None,
                    issue: HealthIssueKind::RecordRejection("missing-model".into()),
                    affected_inputs: 1,
                    rejected_records: Some(3),
                    handling: HealthHandling::RecordSkipped,
                },
                HealthIssue {
                    level: HealthLevel::Error,
                    client: None,
                    issue: HealthIssueKind::PartialInput,
                    affected_inputs: 2,
                    rejected_records: None,
                    handling: HealthHandling::ConfirmedDataKept,
                },
                HealthIssue {
                    level: HealthLevel::Error,
                    client: None,
                    issue: HealthIssueKind::InputUnavailable,
                    affected_inputs: 1,
                    rejected_records: None,
                    handling: HealthHandling::InputSkipped,
                },
            ],
        };

        assert_eq!(report.issue_count(), 6);
    }

    #[test]
    fn health_report_issue_count_saturates_for_untrusted_cached_counts() {
        let report = HealthSummary {
            issues: vec![
                HealthIssue {
                    level: HealthLevel::Warning,
                    client: None,
                    issue: HealthIssueKind::RecordRejection("malformed-record".into()),
                    affected_inputs: 1,
                    rejected_records: Some(u64::MAX),
                    handling: HealthHandling::RecordSkipped,
                },
                HealthIssue {
                    level: HealthLevel::Warning,
                    client: None,
                    issue: HealthIssueKind::InputCacheUnavailable,
                    affected_inputs: u64::MAX,
                    rejected_records: None,
                    handling: HealthHandling::CacheBypassed,
                },
            ],
            ..HealthSummary::default()
        };

        assert_eq!(report.issue_count(), u64::MAX);
    }

    #[test]
    fn health_report_rejects_contradictory_derived_fields_and_handling() {
        let mut contradictory = serde_json::to_value(HealthSummary::default()).unwrap();
        contradictory["complete"] = serde_json::json!(false);
        let error = serde_json::from_value::<HealthSummary>(contradictory).unwrap_err();
        assert!(error.to_string().contains("complete"));

        let mut contradictory = serde_json::to_value(HealthSummary::default()).unwrap();
        contradictory["degradedInputs"] = serde_json::json!(1);
        let error = serde_json::from_value::<HealthSummary>(contradictory).unwrap_err();
        assert!(error.to_string().contains("degraded input count"));

        let mut summary = HealthSummary::default();
        summary.issues.push(HealthIssue {
            level: HealthLevel::Error,
            client: None,
            issue: HealthIssueKind::InputUnavailable,
            affected_inputs: 1,
            rejected_records: None,
            handling: HealthHandling::ConfirmedDataKept,
        });
        let error = summary.validate().unwrap_err();
        assert!(matches!(
            error,
            HealthSummaryValidationError::InvalidHandling { .. }
        ));

        let issue = HealthIssue {
            level: HealthLevel::Error,
            client: Some(ClientId::Amp),
            issue: HealthIssueKind::PartialInput,
            affected_inputs: 1,
            rejected_records: None,
            handling: HealthHandling::ConfirmedDataKept,
        };
        let duplicate = HealthSummary {
            issues: vec![issue.clone(), issue],
            ..HealthSummary::default()
        };
        assert!(matches!(
            duplicate.validate(),
            Err(HealthSummaryValidationError::DuplicateIssue { .. })
        ));
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
        assert_eq!(report.rejected_records(), 2);
        assert_eq!(report.failed_inputs(), 2);
        assert_eq!(report.issues.len(), 2);

        let records = report
            .issues
            .iter()
            .find(|issue| issue.issue == "malformed-record")
            .unwrap();
        assert_eq!(records.level, "warning");
        assert_eq!(records.client, Some(ClientId::Codex));
        assert_eq!(records.affected_inputs, 2);
        assert_eq!(records.rejected_records, Some(2));
        assert_eq!(records.handling, "record-skipped");

        let failures = report
            .issues
            .iter()
            .find(|issue| issue.issue == "input-unavailable")
            .unwrap();
        assert_eq!(failures.level, "error");
        assert_eq!(failures.client, Some(ClientId::Codex));
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

    #[test]
    fn global_cache_diagnostic_is_not_attributed_to_every_client() {
        let mut data_health = DataHealth::default();
        data_health.record(health(InputStatus::Complete, RejectionSummary::default()));
        data_health.record_global_diagnostic(
            PathBuf::from("/tmp/tokenx/input"),
            InputDiagnosticKind::CacheUnavailable,
            InputFailure::new("open shard store", "permission denied"),
        );

        let report = data_health.summarize();
        assert!(!report.complete());
        assert_eq!(report.clean_inputs, 1);
        assert_eq!(report.degraded_inputs, 0);
        assert_eq!(report.issue_count(), 1);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].client, None);
        assert_eq!(
            report.issues[0].issue,
            HealthIssueKind::InputCacheUnavailable
        );

        let value = serde_json::to_value(report).unwrap();
        assert!(value["issues"][0]["client"].is_null());
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains("/tmp/tokenx/input"));
        assert!(!encoded.contains("permission denied"));
    }

    #[test]
    fn typed_health_issue_round_trips_through_json_and_bincode() {
        let issue = HealthIssue {
            level: HealthLevel::Warning,
            client: Some(ClientId::Codex),
            issue: HealthIssueKind::RecordRejection("future-reason".to_string()),
            affected_inputs: 2,
            rejected_records: Some(3),
            handling: HealthHandling::Other("future-handling".to_string()),
        };

        let json = serde_json::to_vec(&issue).unwrap();
        let json_round_trip: HealthIssue = serde_json::from_slice(&json).unwrap();
        assert_eq!(json_round_trip, issue);

        let binary = bincode::serialize(&issue).unwrap();
        let binary_round_trip: HealthIssue = bincode::deserialize(&binary).unwrap();
        assert_eq!(binary_round_trip, issue);
    }
}
