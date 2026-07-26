use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum ProviderId {
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "zai")]
    Zai,
    #[serde(rename = "grok")]
    Grok,
    #[serde(rename = "kimi-coding-plan-key")]
    KimiCodingPlanKey,
    #[serde(rename = "kimi-coding-plan-credential")]
    KimiCodingPlanCredential,
    #[serde(rename = "minimax-token-plan-cn")]
    MiniMaxTokenPlanCn,
    #[serde(rename = "minimax-token-plan-global")]
    MiniMaxTokenPlanGlobal,
}

impl ProviderId {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Zai => "zai",
            Self::Grok => "grok",
            Self::KimiCodingPlanKey => "kimi-coding-plan-key",
            Self::KimiCodingPlanCredential => "kimi-coding-plan-credential",
            Self::MiniMaxTokenPlanCn => "minimax-token-plan-cn",
            Self::MiniMaxTokenPlanGlobal => "minimax-token-plan-global",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Zai => "Z.ai GLM Coding Plan",
            Self::Grok => "Grok",
            Self::KimiCodingPlanKey => "Kimi Coding Plan (key)",
            Self::KimiCodingPlanCredential => "Kimi Coding Plan (credential)",
            Self::MiniMaxTokenPlanCn => "MiniMax Token Plan CN",
            Self::MiniMaxTokenPlanGlobal => "MiniMax Token Plan Global",
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageMetric {
    pub label: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub remaining_label: Option<String>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubscriptionOutput {
    pub provider: ProviderId,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<UsageAccount>,
    pub plan: Option<String>,
    pub email: Option<String>,
    pub metrics: Vec<UsageMetric>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubscriptionPayload {
    pub account: Option<UsageAccount>,
    pub plan: Option<String>,
    pub email: Option<String>,
    pub metrics: Vec<UsageMetric>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageAccount {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscriptionError {
    pub provider_id: Option<ProviderId>,
    pub provider: String,
    pub message: String,
}

impl SubscriptionError {
    pub(crate) fn global(provider: impl Into<String>, error: impl std::fmt::Display) -> Self {
        Self {
            provider_id: None,
            provider: provider.into(),
            message: error.to_string(),
        }
    }

    pub(crate) fn provider(provider: ProviderId, error: impl std::fmt::Display) -> Self {
        Self {
            provider_id: Some(provider),
            provider: provider.label().to_string(),
            message: error.to_string(),
        }
    }

    fn is_for(&self, provider: ProviderId) -> bool {
        self.provider_id == Some(provider)
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SubscriptionBatch {
    pub outputs: Vec<SubscriptionOutput>,
    pub errors: Vec<SubscriptionError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchRequest {
    Started,
    AlreadyFetching,
    NoProviders,
}

#[derive(Debug)]
pub(crate) enum SubscriptionPoll {
    Pending,
    Batch(SubscriptionBatch),
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubscriptionInstall {
    Loaded,
    LoadedWithErrors,
    Empty,
    Failed,
}

enum FetchLifecycle {
    NotStarted,
    Queued {
        since: Instant,
    },
    Running {
        since: Instant,
        receiver: Receiver<SubscriptionBatch>,
    },
    Settled,
}

pub(crate) struct SubscriptionState {
    enabled: Vec<ProviderId>,
    outputs: Vec<SubscriptionOutput>,
    errors: Vec<SubscriptionError>,
    lifecycle: FetchLifecycle,
    last_checked: Option<Instant>,
}

impl SubscriptionState {
    pub(crate) fn new(
        enabled: Vec<ProviderId>,
        cached: Result<Option<Vec<SubscriptionOutput>>>,
    ) -> Self {
        let (outputs, errors) = match cached {
            Ok(Some(outputs)) if enabled.is_empty() => (outputs, Vec::new()),
            Ok(Some(outputs)) => (
                enabled
                    .iter()
                    .filter_map(|provider| {
                        outputs
                            .iter()
                            .find(|output| output.provider == *provider)
                            .cloned()
                    })
                    .collect(),
                Vec::new(),
            ),
            Ok(None) => (Vec::new(), Vec::new()),
            Err(error) => (
                Vec::new(),
                vec![SubscriptionError::global("Subscription cache", error)],
            ),
        };
        Self {
            enabled,
            outputs,
            errors,
            lifecycle: FetchLifecycle::NotStarted,
            last_checked: None,
        }
    }

    pub(crate) fn disabled() -> Self {
        Self::new(Vec::new(), Ok(None))
    }

    pub(crate) fn outputs(&self) -> &[SubscriptionOutput] {
        &self.outputs
    }

    pub(crate) fn errors(&self) -> &[SubscriptionError] {
        &self.errors
    }

    pub(crate) fn enabled(&self) -> &[ProviderId] {
        &self.enabled
    }

    pub(crate) fn has_fetch_history(&self) -> bool {
        !matches!(self.lifecycle, FetchLifecycle::NotStarted)
    }

    pub(crate) fn last_checked(&self) -> Option<Instant> {
        self.last_checked
    }

    pub(crate) fn is_fetching(&self) -> bool {
        matches!(
            self.lifecycle,
            FetchLifecycle::Queued { .. } | FetchLifecycle::Running { .. }
        )
    }

    pub(crate) fn fetch_elapsed(&self) -> Option<Duration> {
        match &self.lifecycle {
            FetchLifecycle::Queued { since } | FetchLifecycle::Running { since, .. } => {
                Some(since.elapsed())
            }
            FetchLifecycle::NotStarted | FetchLifecycle::Settled => None,
        }
    }

    pub(crate) fn request_fetch(&mut self) -> FetchRequest {
        if self.is_fetching() {
            return FetchRequest::AlreadyFetching;
        }
        if self.enabled.is_empty() {
            return FetchRequest::NoProviders;
        }
        self.lifecycle = FetchLifecycle::Queued {
            since: Instant::now(),
        };
        FetchRequest::Started
    }

    pub(crate) fn take_request(&mut self) -> Option<(Vec<ProviderId>, Sender<SubscriptionBatch>)> {
        let since = match std::mem::replace(&mut self.lifecycle, FetchLifecycle::NotStarted) {
            FetchLifecycle::Queued { since } => since,
            lifecycle => {
                self.lifecycle = lifecycle;
                return None;
            }
        };
        let (sender, receiver) = mpsc::channel();
        self.lifecycle = FetchLifecycle::Running { since, receiver };
        Some((self.enabled.clone(), sender))
    }

    pub(crate) fn poll(&mut self) -> SubscriptionPoll {
        let FetchLifecycle::Running { receiver, .. } = &self.lifecycle else {
            return SubscriptionPoll::Pending;
        };
        match receiver.try_recv() {
            Ok(batch) => {
                self.lifecycle = FetchLifecycle::Settled;
                SubscriptionPoll::Batch(batch)
            }
            Err(TryRecvError::Disconnected) => {
                self.lifecycle = FetchLifecycle::Settled;
                SubscriptionPoll::Disconnected
            }
            Err(TryRecvError::Empty) => SubscriptionPoll::Pending,
        }
    }

    pub(crate) fn install(
        &mut self,
        batch: SubscriptionBatch,
        persist: impl FnOnce(&[SubscriptionOutput]) -> Result<()>,
    ) -> SubscriptionInstall {
        let mut errors = batch.errors;
        self.last_checked = Some(Instant::now());
        let previous = std::mem::take(&mut self.outputs);
        let mut refreshed = batch.outputs;
        for output in &mut refreshed {
            output.stale = false;
        }
        let merge_order = if self.enabled.is_empty() {
            let mut providers = previous
                .iter()
                .map(|output| output.provider)
                .collect::<Vec<_>>();
            for provider in refreshed.iter().map(|output| output.provider) {
                if !providers.contains(&provider) {
                    providers.push(provider);
                }
            }
            providers
        } else {
            self.enabled.clone()
        };
        self.outputs = merge_order
            .into_iter()
            .filter_map(|provider| {
                refreshed
                    .iter()
                    .find(|output| output.provider == provider)
                    .cloned()
                    .or_else(|| {
                        let failed = errors.iter().any(|error| error.is_for(provider));
                        let missing_during_failed_batch = !errors.is_empty()
                            && refreshed.iter().all(|output| output.provider != provider);
                        (failed || missing_during_failed_batch)
                            .then(|| {
                                previous
                                    .iter()
                                    .find(|output| output.provider == provider)
                                    .cloned()
                            })
                            .flatten()
                            .map(|mut output| {
                                output.stale = true;
                                output
                            })
                    })
            })
            .collect();
        if !self.outputs.is_empty() {
            if let Err(error) = persist(&self.outputs) {
                errors.push(SubscriptionError::global("Subscription cache", error));
            }
            self.errors = errors;
            if self.errors.is_empty() {
                SubscriptionInstall::Loaded
            } else {
                SubscriptionInstall::LoadedWithErrors
            }
        } else {
            self.errors = errors;
            if self.errors.is_empty() {
                SubscriptionInstall::Empty
            } else {
                SubscriptionInstall::Failed
            }
        }
    }

    pub(crate) fn install_disconnected(&mut self) {
        self.last_checked = Some(Instant::now());
        self.errors = vec![SubscriptionError::global(
            "unknown",
            "subscription fetch worker disconnected",
        )];
    }

    pub(crate) fn should_start_initial_fetch(&self, subscription_tab_active: bool) -> bool {
        subscription_tab_active
            && matches!(self.lifecycle, FetchLifecycle::NotStarted)
            && !self.enabled.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn replace_outputs_for_test(&mut self, outputs: Vec<SubscriptionOutput>) {
        self.outputs = outputs;
    }

    #[cfg(test)]
    pub(crate) fn replace_errors_for_test(&mut self, errors: Vec<SubscriptionError>) {
        self.errors = errors;
    }

    #[cfg(test)]
    pub(crate) fn set_enabled_for_test(&mut self, enabled: Vec<ProviderId>) {
        self.enabled = enabled;
    }

    #[cfg(test)]
    pub(crate) fn start_fetch_for_test(&mut self, receiver: Receiver<SubscriptionBatch>) {
        self.lifecycle = FetchLifecycle::Running {
            since: Instant::now(),
            receiver,
        };
    }

    #[cfg(test)]
    pub(crate) fn outputs_mut_for_test(&mut self) -> &mut Vec<SubscriptionOutput> {
        &mut self.outputs
    }

    #[cfg(test)]
    pub(crate) fn set_last_checked_for_test(&mut self, checked: Option<Instant>) {
        self.last_checked = checked;
    }
}

impl UsageAccount {
    pub(crate) fn label_name(&self) -> Option<&str> {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
    }

    pub(crate) fn short_id(&self) -> String {
        let id = self.id.trim();
        if id.is_empty() {
            return "unknown".to_string();
        }
        if id.chars().count() <= 12 {
            return id.to_string();
        }
        let head = id.chars().take(6).collect::<String>();
        let tail = id
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        format!("{head}...{tail}")
    }

    pub(crate) fn display_name(&self) -> String {
        self.label_name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("Account {}", self.short_id()))
    }
}

impl SubscriptionOutput {
    pub(super) fn new(provider: ProviderId, payload: SubscriptionPayload) -> Self {
        Self {
            provider,
            stale: false,
            account: payload.account,
            plan: payload.plan,
            email: payload.email,
            metrics: payload.metrics,
        }
    }

    pub(crate) fn account_display_name(&self) -> Option<String> {
        let account = self.account.as_ref()?;
        if let Some(label) = account.label_name() {
            return Some(label.to_string());
        }
        if let Some(email) = self
            .email
            .as_deref()
            .map(str::trim)
            .filter(|email| !email.is_empty())
        {
            return Some(email.to_string());
        }
        Some(account.display_name())
    }

    pub(crate) fn display_name(&self) -> String {
        let display_name = match self.account {
            Some(_) => format!(
                "{} ({})",
                self.provider.label(),
                self.account_display_name().unwrap_or_default()
            ),
            None => self.provider.label().to_string(),
        };
        if self.stale {
            format!("{display_name} (stale)")
        } else {
            display_name
        }
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn provider_ids_serialize_as_their_canonical_settings_ids() {
        let providers = [
            ProviderId::Claude,
            ProviderId::Codex,
            ProviderId::Zai,
            ProviderId::Grok,
            ProviderId::KimiCodingPlanKey,
            ProviderId::KimiCodingPlanCredential,
            ProviderId::MiniMaxTokenPlanCn,
            ProviderId::MiniMaxTokenPlanGlobal,
        ];

        for provider in providers {
            assert_eq!(
                serde_json::to_value(provider).unwrap(),
                serde_json::Value::String(provider.as_str().to_string())
            );
            assert_eq!(
                serde_json::from_value::<ProviderId>(serde_json::Value::String(
                    provider.as_str().to_string()
                ))
                .unwrap(),
                provider
            );
            assert!(!provider.label().is_empty());
        }
    }

    #[test]
    fn output_identity_is_attached_to_an_identity_neutral_payload() {
        let output = SubscriptionOutput::new(
            ProviderId::Codex,
            SubscriptionPayload {
                account: None,
                plan: Some("Pro".to_string()),
                email: None,
                metrics: Vec::new(),
            },
        );

        assert_eq!(output.provider, ProviderId::Codex);
        assert_eq!(output.display_name(), ProviderId::Codex.label());
        assert_eq!(output.plan.as_deref(), Some("Pro"));
    }

    #[test]
    fn fetch_lifecycle_is_linear_and_allows_a_later_manual_refresh() {
        let mut state = SubscriptionState::new(vec![ProviderId::Codex], Ok(None));
        assert!(state.should_start_initial_fetch(true));
        assert!(!state.has_fetch_history());

        assert_eq!(state.request_fetch(), FetchRequest::Started);
        assert_eq!(state.request_fetch(), FetchRequest::AlreadyFetching);
        assert!(state.is_fetching());

        let (_, sender) = state.take_request().expect("queued request");
        assert!(state.take_request().is_none());
        assert!(matches!(state.poll(), SubscriptionPoll::Pending));

        sender.send(SubscriptionBatch::default()).unwrap();
        assert!(matches!(
            state.poll(),
            SubscriptionPoll::Batch(SubscriptionBatch { .. })
        ));
        assert!(!state.is_fetching());
        assert!(state.has_fetch_history());
        assert!(!state.should_start_initial_fetch(true));

        assert_eq!(state.request_fetch(), FetchRequest::Started);
    }

    #[test]
    fn empty_provider_set_never_enters_the_fetch_lifecycle() {
        let mut state = SubscriptionState::disabled();

        assert_eq!(state.request_fetch(), FetchRequest::NoProviders);
        assert!(state.take_request().is_none());
        assert!(matches!(state.poll(), SubscriptionPoll::Pending));
        assert!(!state.has_fetch_history());
        assert!(!state.should_start_initial_fetch(true));
    }

    #[test]
    fn nonempty_allowlist_filters_and_orders_cached_outputs() {
        let payload = || SubscriptionPayload {
            account: None,
            plan: None,
            email: None,
            metrics: Vec::new(),
        };
        let cached = vec![
            SubscriptionOutput::new(ProviderId::Claude, payload()),
            SubscriptionOutput::new(ProviderId::Codex, payload()),
            SubscriptionOutput::new(ProviderId::Zai, payload()),
        ];

        let state =
            SubscriptionState::new(vec![ProviderId::Zai, ProviderId::Codex], Ok(Some(cached)));

        assert_eq!(
            state
                .outputs()
                .iter()
                .map(|output| output.provider)
                .collect::<Vec<_>>(),
            vec![ProviderId::Zai, ProviderId::Codex]
        );
    }

    #[test]
    fn empty_allowlist_keeps_all_cached_outputs_for_cache_display_mode() {
        let payload = || SubscriptionPayload {
            account: None,
            plan: None,
            email: None,
            metrics: Vec::new(),
        };
        let cached = vec![
            SubscriptionOutput::new(ProviderId::Claude, payload()),
            SubscriptionOutput::new(ProviderId::Codex, payload()),
        ];

        let state = SubscriptionState::new(Vec::new(), Ok(Some(cached)));

        assert_eq!(state.outputs().len(), 2);
    }

    #[test]
    fn disconnected_worker_settles_the_lifecycle() {
        let mut state = SubscriptionState::new(vec![ProviderId::Codex], Ok(None));
        assert_eq!(state.request_fetch(), FetchRequest::Started);
        let (_, sender) = state.take_request().expect("queued request");
        drop(sender);

        assert!(matches!(state.poll(), SubscriptionPoll::Disconnected));
        assert!(!state.is_fetching());
        assert!(!state.should_start_initial_fetch(true));
    }

    #[test]
    fn partial_refresh_replaces_successes_and_marks_failed_provider_snapshot_stale() {
        let cached_codex = SubscriptionOutput::new(
            ProviderId::Codex,
            SubscriptionPayload {
                account: None,
                plan: Some("Old Codex".to_string()),
                email: None,
                metrics: Vec::new(),
            },
        );
        let cached_claude = SubscriptionOutput::new(
            ProviderId::Claude,
            SubscriptionPayload {
                account: None,
                plan: Some("Old Claude".to_string()),
                email: None,
                metrics: Vec::new(),
            },
        );
        let refreshed_codex = SubscriptionOutput::new(
            ProviderId::Codex,
            SubscriptionPayload {
                account: None,
                plan: Some("New Codex".to_string()),
                email: None,
                metrics: Vec::new(),
            },
        );
        let mut state = SubscriptionState::new(
            vec![ProviderId::Codex, ProviderId::Claude],
            Ok(Some(vec![cached_codex, cached_claude])),
        );

        let install = state.install(
            SubscriptionBatch {
                outputs: vec![refreshed_codex],
                errors: vec![SubscriptionError::provider(
                    ProviderId::Claude,
                    "credential expired",
                )],
            },
            |_| Ok(()),
        );

        assert_eq!(install, SubscriptionInstall::LoadedWithErrors);
        assert_eq!(state.outputs.len(), 2);
        assert_eq!(state.outputs[0].plan.as_deref(), Some("New Codex"));
        assert!(!state.outputs[0].stale);
        assert_eq!(state.outputs[1].plan.as_deref(), Some("Old Claude"));
        assert!(state.outputs[1].stale);
        assert_eq!(state.outputs[1].display_name(), "Claude (stale)");
    }
}
