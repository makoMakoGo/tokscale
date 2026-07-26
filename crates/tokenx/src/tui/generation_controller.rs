use std::panic;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::acquisition::{acquisition_engine, build_generation_with_cancellation};
use crate::cli::RelativeDateRange;
use crate::generation_cache::{save_generation_cache_with_retry_backoff, RetryBackoff};

use super::app::{App, StatusTone};
use super::task_supervisor::TaskSupervisor;
use crate::settings::{AUTO_REFRESH_STEP_MS, MAX_AUTO_REFRESH_MS, MIN_AUTO_REFRESH_MS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshRequest {
    Automatic,
    Manual,
}

impl RefreshRequest {
    fn force(self) -> bool {
        matches!(self, Self::Manual)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshControl {
    ToggleAutomatic,
    IncreaseInterval,
    DecreaseInterval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefreshStatus {
    automatic: bool,
    interval: Duration,
    elapsed: Duration,
    loading: bool,
    loading_elapsed: Duration,
}

impl RefreshStatus {
    pub(crate) fn new(automatic: bool, interval: Duration, elapsed: Duration) -> Self {
        Self {
            automatic,
            interval,
            elapsed,
            loading: false,
            loading_elapsed: Duration::ZERO,
        }
    }

    pub(crate) fn automatic(self) -> bool {
        self.automatic
    }

    pub(crate) fn interval(self) -> Duration {
        self.interval
    }

    pub(crate) fn elapsed(self) -> Duration {
        self.elapsed
    }

    pub(crate) fn loading(self) -> bool {
        self.loading
    }

    pub(crate) fn loading_elapsed(self) -> Option<Duration> {
        self.loading.then_some(self.loading_elapsed)
    }

    #[cfg(test)]
    pub(crate) fn set_loading_for_test(&mut self, loading: bool) {
        self.loading = loading;
        self.loading_elapsed = Duration::ZERO;
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingRefresh {
    request: RefreshRequest,
}

#[derive(Debug, Clone, Copy)]
struct ActiveRefresh {
    id: u64,
    force: bool,
    started_at: Instant,
}

pub(super) enum BackgroundLoad {
    Unchanged,
    Loaded {
        generation: Box<tokenx_engine::Generation>,
        cache_persistence_warning: Option<String>,
        retry_backoff: Option<RetryBackoff>,
    },
}

pub(super) struct AcquisitionTaskResult {
    pub(super) request_id: u64,
    pub(super) result: Result<BackgroundLoad>,
}

/// The sole owner of local acquisition and refresh lifecycle state.
///
/// `App` emits typed UI intent and receives a presentation snapshot. It never
/// owns a loader, source fingerprint, refresh clock, or in-flight authority.
pub(super) struct GenerationController {
    acquisition: tokenx_engine::AcquisitionEngine,
    status: RefreshStatus,
    last_checked: Instant,
    pending: Option<PendingRefresh>,
    active: Option<ActiveRefresh>,
    next_request_id: u64,
    retry_backoff: Option<RetryBackoff>,
    relative_date_range: Option<RelativeDateRange>,
}

impl GenerationController {
    pub(super) fn new(
        acquisition: tokenx_engine::AcquisitionEngine,
        status: RefreshStatus,
    ) -> Self {
        Self {
            acquisition,
            status,
            last_checked: Instant::now(),
            pending: None,
            active: None,
            next_request_id: 1,
            retry_backoff: None,
            relative_date_range: None,
        }
    }

    pub(super) fn with_relative_date_range(
        mut self,
        relative_date_range: Option<RelativeDateRange>,
    ) -> Self {
        self.relative_date_range = relative_date_range;
        self
    }

    pub(super) fn set_retry_backoff(&mut self, retry_backoff: Option<RetryBackoff>) {
        if let Some(backoff) = retry_backoff.as_ref() {
            tracing::debug!(
                retry_attempt = backoff.attempt(),
                retry_affected_clients = ?backoff.affected_clients(),
                "installed acquisition retry backoff"
            );
        }
        self.retry_backoff = retry_backoff;
    }

    pub(super) fn request_initial_load(&mut self, force: bool) {
        self.queue(PendingRefresh {
            request: if force {
                RefreshRequest::Manual
            } else {
                RefreshRequest::Automatic
            },
        });
    }

    pub(super) fn consume_app_intents(&mut self, app: &mut App) {
        for control in app.take_refresh_controls() {
            self.apply_control(app, control);
        }
        for request in app.take_refresh_requests() {
            self.queue(PendingRefresh { request });
        }
        self.publish_status(app);
    }

    pub(super) fn on_tick(&mut self, app: &mut App, now: Instant) {
        self.on_tick_for_date(
            app,
            now,
            self.acquisition.config().calendar().current_date(),
        );
    }

    fn on_tick_for_date(&mut self, app: &mut App, now: Instant, effective_date: chrono::NaiveDate) {
        let date_changed = app.effective_date() != effective_date;
        app.advance_effective_date(effective_date);
        if date_changed && self.relative_date_range.is_some() {
            self.queue(PendingRefresh {
                request: RefreshRequest::Automatic,
            });
        }
        if self.status.automatic
            && now.saturating_duration_since(self.last_checked) >= self.status.interval
            && self.active.is_none()
            && self.pending.is_none()
        {
            self.queue(PendingRefresh {
                request: RefreshRequest::Automatic,
            });
        }
        self.publish_status(app);
    }

    pub(super) fn start_pending(&mut self, app: &mut App, tasks: &mut TaskSupervisor) {
        if self.active.is_some() {
            return;
        }
        let Some(pending) = self.pending.take() else {
            return;
        };

        let context_changed = match self.refresh_acquisition_context(app.effective_date()) {
            Ok(changed) => changed,
            Err(error) => {
                generation_background_failure(
                    app,
                    format!("Failed to refresh acquisition context: {error:#}"),
                );
                self.publish_status(app);
                return;
            }
        };
        let force = should_force_input_reload(
            pending.request.force() || context_changed,
            app.generation_health(),
            self.retry_backoff.as_ref(),
        );
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .expect("refresh request id overflow");
        self.active = Some(ActiveRefresh {
            id: request_id,
            force,
            started_at: Instant::now(),
        });
        self.publish_status(app);
        let last_fingerprint = installed_source_fingerprint(app);
        tasks.spawn_acquisition(
            request_id,
            self.acquisition.clone(),
            force,
            last_fingerprint,
        );
    }

    fn refresh_acquisition_context(&mut self, effective_date: chrono::NaiveDate) -> Result<bool> {
        let current = self.acquisition.config();
        let date_range = self
            .relative_date_range
            .map(|relative| relative.resolve(effective_date))
            .unwrap_or_else(|| current.date_range().clone());
        let replacement = acquisition_engine(
            current.resolved_home_dir().to_path_buf(),
            current.universe().clone(),
            date_range,
            current.scanner().clone(),
            *current.calendar(),
            self.acquisition.pricing_snapshot(),
        )?;
        if replacement.config() == current {
            return Ok(false);
        }

        self.acquisition = replacement;
        self.retry_backoff = None;
        Ok(true)
    }

    pub(super) fn apply_task_result(
        &mut self,
        app: &mut App,
        completed: AcquisitionTaskResult,
    ) -> bool {
        let Some(active) = self.active else {
            tracing::warn!(
                request_id = completed.request_id,
                "ignored acquisition result without an active request"
            );
            return false;
        };
        if completed.request_id != active.id {
            tracing::warn!(
                request_id = completed.request_id,
                active_request_id = active.id,
                "ignored stale acquisition result"
            );
            return false;
        }
        self.active = None;
        self.last_checked = Instant::now();

        match completed.result {
            Ok(BackgroundLoad::Loaded {
                generation,
                cache_persistence_warning,
                retry_backoff,
            }) => {
                if let Err(error) = app.install_generation(*generation) {
                    generation_background_failure(
                        app,
                        format!("Generation projection failed: {error:#}"),
                    );
                } else {
                    self.set_retry_backoff(retry_backoff);
                    let recovered_cache_warning = cache_persistence_warning
                        .is_none()
                        .then(|| app.generation_cache_warning().map(str::to_owned))
                        .flatten();
                    app.set_generation_cache_warning(cache_persistence_warning);
                    if let Some(warning) = recovered_cache_warning {
                        app.set_generation_status_with_tone(&warning, StatusTone::Warning);
                    } else {
                        app.set_generation_status_with_tone("Data loaded", StatusTone::Success);
                    }
                }
            }
            Ok(BackgroundLoad::Unchanged) if active.force => {
                generation_background_failure(
                    app,
                    "Forced acquisition returned an illegal unchanged result".to_string(),
                );
            }
            Ok(BackgroundLoad::Unchanged) => {}
            Err(error) => {
                generation_background_failure(app, format!("{error:#}"));
            }
        }
        self.publish_status(app);
        true
    }

    fn queue(&mut self, pending: PendingRefresh) {
        match (self.pending.as_mut(), pending.request) {
            (Some(queued), RefreshRequest::Manual) => {
                queued.request = RefreshRequest::Manual;
            }
            (Some(_), RefreshRequest::Automatic) => {}
            (None, _) => self.pending = Some(pending),
        }
    }

    fn apply_control(&mut self, app: &mut App, control: RefreshControl) {
        let message = match control {
            RefreshControl::ToggleAutomatic => {
                self.status.automatic = !self.status.automatic;
                if self.status.automatic {
                    self.last_checked = Instant::now();
                }
                if self.status.automatic {
                    format!("Auto-refresh ON ({}s)", self.status.interval.as_secs())
                } else {
                    "Auto-refresh OFF".to_string()
                }
            }
            RefreshControl::IncreaseInterval => {
                let millis = self.status.interval.as_millis() as u64;
                self.status.interval = Duration::from_millis(
                    millis
                        .saturating_add(AUTO_REFRESH_STEP_MS)
                        .min(MAX_AUTO_REFRESH_MS),
                );
                format!("Refresh interval: {}s", self.status.interval.as_secs())
            }
            RefreshControl::DecreaseInterval => {
                let millis = self.status.interval.as_millis() as u64;
                self.status.interval = Duration::from_millis(
                    millis
                        .saturating_sub(AUTO_REFRESH_STEP_MS)
                        .max(MIN_AUTO_REFRESH_MS),
                );
                format!("Refresh interval: {}s", self.status.interval.as_secs())
            }
        };
        app.persist_refresh_policy(self.status.automatic, self.status.interval, message);
        self.publish_status(app);
    }

    fn publish_status(&mut self, app: &mut App) {
        self.status.elapsed = self.last_checked.elapsed();
        self.status.loading = self.active.is_some();
        self.status.loading_elapsed = self
            .active
            .map(|active| active.started_at.elapsed())
            .unwrap_or_default();
        app.set_refresh_status(self.status);
    }

    #[cfg(test)]
    pub(super) fn apply_result_for_test(
        &mut self,
        app: &mut App,
        result: Result<BackgroundLoad>,
        force: bool,
    ) {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        self.active = Some(ActiveRefresh {
            id: request_id,
            force,
            started_at: Instant::now(),
        });
        assert!(self.apply_task_result(app, AcquisitionTaskResult { request_id, result }));
    }
}

fn generation_background_failure(app: &mut App, diagnostic: String) {
    app.fail_local_usage_load(diagnostic.clone());
    app.set_generation_status_with_tone(&format!("Error: {diagnostic}"), StatusTone::Danger);
}

fn should_force_input_reload(
    explicitly_requested: bool,
    health: Option<&tokenx_engine::input_health::HealthSummary>,
    retry_backoff: Option<&RetryBackoff>,
) -> bool {
    explicitly_requested
        || (health.is_some_and(|health| health.requires_input_retry())
            && retry_backoff.is_none_or(RetryBackoff::is_due))
}

fn installed_source_fingerprint(app: &App) -> Option<tokenx_engine::SourceFingerprint> {
    app.installed_generation()
        .map(|installed| installed.generation().source_fingerprint())
}

#[cfg(test)]
pub(super) fn load_background_data(
    engine: &tokenx_engine::AcquisitionEngine,
    force: bool,
    last_fingerprint: Option<tokenx_engine::SourceFingerprint>,
) -> Result<BackgroundLoad> {
    load_background_data_with_cancellation(
        engine,
        force,
        last_fingerprint,
        &tokenx_engine::AcquisitionCancellation::default(),
    )
}

pub(super) fn load_background_data_with_cancellation(
    engine: &tokenx_engine::AcquisitionEngine,
    force: bool,
    last_fingerprint: Option<tokenx_engine::SourceFingerprint>,
    cancellation: &tokenx_engine::AcquisitionCancellation,
) -> Result<BackgroundLoad> {
    let prepared = engine.prepare_with_cancellation(cancellation)?;
    let fingerprint = prepared.source_fingerprint();
    if !force && last_fingerprint == Some(fingerprint) {
        return Ok(BackgroundLoad::Unchanged);
    }

    build_generation_with_cancellation(engine, prepared, cancellation).map(|generation| {
        BackgroundLoad::Loaded {
            generation: Box::new(generation),
            cache_persistence_warning: None,
            retry_backoff: None,
        }
    })
}

#[cfg(test)]
pub(super) fn persist_background_load(result: Result<BackgroundLoad>) -> Result<BackgroundLoad> {
    persist_background_load_with_cancellation(
        result,
        &tokenx_engine::AcquisitionCancellation::default(),
    )
}

pub(super) fn persist_background_load_with_cancellation(
    result: Result<BackgroundLoad>,
    cancellation: &tokenx_engine::AcquisitionCancellation,
) -> Result<BackgroundLoad> {
    let result = result?;
    cancellation
        .check(tokenx_engine::AcquisitionPhase::GenerationFinalization)
        .map_err(anyhow::Error::new)?;
    let BackgroundLoad::Loaded {
        generation,
        cache_persistence_warning: _,
        retry_backoff: _,
    } = result
    else {
        return Ok(BackgroundLoad::Unchanged);
    };

    match save_generation_cache_with_retry_backoff(&generation) {
        Ok(retry_backoff) => Ok(BackgroundLoad::Loaded {
            generation,
            cache_persistence_warning: None,
            retry_backoff,
        }),
        Err(error) => {
            let diagnostic = format!("{error:#}");
            tracing::warn!(
                error = %diagnostic,
                "local generation loaded but cache persistence failed"
            );
            Ok(BackgroundLoad::Loaded {
                generation,
                cache_persistence_warning: Some(format!("Cache persistence warning: {diagnostic}")),
                retry_backoff: None,
            })
        }
    }
}

#[cfg(test)]
pub(super) fn run_acquisition_task(
    tx: &mpsc::Sender<AcquisitionTaskResult>,
    request_id: u64,
    task: impl FnOnce() -> Result<BackgroundLoad>,
) {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(task)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("unknown panic payload");
        Err(anyhow::anyhow!("TUI background worker panicked: {message}"))
    });
    if tx
        .send(AcquisitionTaskResult { request_id, result })
        .is_err()
    {
        tracing::warn!(
            request_id,
            "dropped TUI background load result because receiver is closed"
        );
    }
}

pub(super) fn run_acquisition_task_with_cancellation(
    tx: &mpsc::Sender<AcquisitionTaskResult>,
    request_id: u64,
    cancellation: &tokenx_engine::AcquisitionCancellation,
    task: impl FnOnce() -> Result<BackgroundLoad>,
) {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(task)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("unknown panic payload");
        Err(anyhow::anyhow!("TUI background worker panicked: {message}"))
    });
    if cancellation.is_cancelled() {
        return;
    }
    if tx
        .send(AcquisitionTaskResult { request_id, result })
        .is_err()
    {
        tracing::warn!(
            request_id,
            "dropped TUI background load result because receiver is closed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{Tab, TuiConfig};
    use crate::tui::local_usage::LocalUsageStatus;
    use tokenx_engine::ClientId;

    fn harness(automatic: bool) -> (App, GenerationController) {
        let universe = tokenx_engine::ClientUniverse::new([tokenx_engine::ClientId::Amp]).unwrap();
        let config = TuiConfig {
            theme: Some(crate::theme::ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: universe.clone(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let mut app = App::new_for_test(config).unwrap();
        let status = RefreshStatus::new(automatic, Duration::from_secs(30), Duration::ZERO);
        app.set_refresh_status(status);
        let acquisition = crate::acquisition::acquisition_engine(
            std::path::PathBuf::from("/tmp/tokenx-generation-controller-test"),
            universe,
            tokenx_engine::DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
            tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
            crate::acquisition::test_pricing_snapshot(),
        )
        .unwrap();
        let controller = GenerationController::new(acquisition, status);
        (app, controller)
    }

    #[test]
    fn manual_request_supersedes_queued_automatic_request() {
        let (_, mut controller) = harness(false);
        controller.queue(PendingRefresh {
            request: RefreshRequest::Automatic,
        });
        controller.queue(PendingRefresh {
            request: RefreshRequest::Manual,
        });
        assert!(controller.pending.unwrap().request.force());
    }

    #[test]
    fn force_rule_includes_degraded_generation_health() {
        let degraded = tokenx_engine::input_health::HealthSummary {
            issues: vec![tokenx_engine::input_health::HealthIssue {
                level: tokenx_engine::input_health::HealthLevel::Error,
                client: Some(ClientId::Amp),
                issue: tokenx_engine::input_health::HealthIssueKind::InputUnavailable,
                affected_inputs: 1,
                rejected_records: None,
                handling: tokenx_engine::input_health::HealthHandling::InputSkipped,
            }],
            ..Default::default()
        };
        assert!(should_force_input_reload(false, Some(&degraded), None));
        assert!(should_force_input_reload(true, None, None));
        assert!(!should_force_input_reload(false, None, None));
    }

    #[test]
    fn automatic_refresh_is_independent_of_the_current_tab() {
        let (mut app, mut controller) = harness(true);
        app.current_tab = Tab::Subscription;
        controller.last_checked = Instant::now() - Duration::from_secs(31);

        controller.on_tick(&mut app, Instant::now());

        assert!(matches!(
            controller.pending,
            Some(PendingRefresh {
                request: RefreshRequest::Automatic
            })
        ));
    }

    #[test]
    fn relative_range_rebinds_when_refresh_starts_after_midnight() {
        let (_, controller) = harness(false);
        let mut controller =
            controller.with_relative_date_range(Some(RelativeDateRange::LastSevenDays));
        let calendar = *controller.acquisition.config().calendar();
        let pricing = controller.acquisition.config().pricing().clone();
        let pricing_snapshot = controller.acquisition.pricing_snapshot();
        let next_day = chrono::NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();

        assert!(controller.refresh_acquisition_context(next_day).unwrap());

        assert_eq!(
            controller.acquisition.config().date_range(),
            &RelativeDateRange::LastSevenDays.resolve(next_day)
        );
        assert_eq!(controller.acquisition.config().calendar(), &calendar);
        assert_eq!(controller.acquisition.config().pricing(), &pricing);
        assert!(std::sync::Arc::ptr_eq(
            &controller.acquisition.pricing_snapshot(),
            &pricing_snapshot,
        ));
    }

    #[test]
    fn midnight_projection_change_does_not_queue_an_input_rescan() {
        let (mut app, mut controller) = harness(false);
        let next_day = chrono::NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();

        controller.on_tick_for_date(&mut app, Instant::now(), next_day);

        assert_eq!(app.effective_date(), next_day);
        assert!(controller.pending.is_none());
        assert!(controller.active.is_none());
    }

    #[test]
    fn relative_range_midnight_change_queues_a_refresh_even_when_automatic_refresh_is_off() {
        let (mut app, controller) = harness(false);
        let mut controller =
            controller.with_relative_date_range(Some(RelativeDateRange::LastSevenDays));
        let next_day = chrono::NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();

        controller.on_tick_for_date(&mut app, Instant::now(), next_day);

        assert_eq!(app.effective_date(), next_day);
        assert!(matches!(
            controller.pending,
            Some(PendingRefresh {
                request: RefreshRequest::Automatic
            })
        ));
        assert!(controller.active.is_none());
    }

    #[test]
    fn stale_and_illegal_results_cannot_mutate_generation_state() {
        let (mut app, mut controller) = harness(false);
        controller.active = Some(ActiveRefresh {
            id: 2,
            force: true,
            started_at: Instant::now(),
        });
        assert!(!controller.apply_task_result(
            &mut app,
            AcquisitionTaskResult {
                request_id: 1,
                result: Err(anyhow::anyhow!("stale")),
            },
        ));
        assert_eq!(controller.active.unwrap().id, 2);
        assert_eq!(app.local_usage_status(), LocalUsageStatus::Empty);

        assert!(controller.apply_task_result(
            &mut app,
            AcquisitionTaskResult {
                request_id: 2,
                result: Ok(BackgroundLoad::Unchanged),
            },
        ));
        assert!(matches!(
            app.local_usage_status(),
            LocalUsageStatus::Failed { diagnostic }
                if diagnostic.contains("illegal unchanged")
        ));
        assert!(!app.is_background_loading());
    }

    #[test]
    fn installed_generation_is_the_complete_source_fingerprint_authority() {
        let (mut app, _) = harness(false);
        assert_eq!(installed_source_fingerprint(&app), None);
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            tokenx_engine::InputFootprint::default(),
        );
        assert_eq!(
            installed_source_fingerprint(&app),
            app.generation_for_test()
                .map(tokenx_engine::Generation::source_fingerprint)
        );
    }

    #[test]
    fn recovered_startup_cache_failure_remains_visible_as_transient_warning() {
        let (mut app, mut controller) = harness(false);
        let warning = "Generation cache warning: decode failure: digest mismatch";
        app.set_generation_cache_warning(Some(warning.to_string()));
        let generation = crate::tui::generation_fixture_with_health(
            [tokenx_engine::ClientId::Amp],
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            tokenx_engine::InputFootprint::default(),
            tokenx_engine::input_health::HealthSummary::default(),
        );

        controller.apply_result_for_test(
            &mut app,
            Ok(BackgroundLoad::Loaded {
                generation: Box::new(generation),
                cache_persistence_warning: None,
                retry_backoff: None,
            }),
            true,
        );

        assert_eq!(app.generation_cache_warning(), None);
        assert_eq!(app.status_message.as_deref(), Some(warning));
        assert_eq!(app.status_message_tone(), StatusTone::Warning);
    }

    #[test]
    fn cancelled_worker_does_not_publish_a_background_result() {
        let (tx, rx) = mpsc::channel();
        let cancellation = tokenx_engine::AcquisitionCancellation::default();
        cancellation.cancel();

        run_acquisition_task_with_cancellation(&tx, 7, &cancellation, || {
            Ok(BackgroundLoad::Unchanged)
        });

        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
}
