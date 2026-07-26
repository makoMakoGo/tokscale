use std::panic;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::acquisition::build_generation;
use crate::generation_cache::save_generation_cache;

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
        }
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

        let force = should_force_input_reload(pending.request.force(), app.generation_health());
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
        let last_digest = installed_source_digest(app);
        tasks.spawn_acquisition(request_id, self.acquisition.clone(), force, last_digest);
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
            }) => {
                if let Err(error) = app.install_generation(*generation) {
                    generation_background_failure(
                        app,
                        format!("Generation projection failed: {error:#}"),
                    );
                } else {
                    app.set_cache_persistence_warning(cache_persistence_warning);
                    app.set_generation_status_with_tone("Data loaded", StatusTone::Success);
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
) -> bool {
    explicitly_requested || health.is_some_and(|health| health.requires_input_retry())
}

fn installed_source_digest(app: &App) -> Option<u64> {
    app.installed_generation()
        .map(|installed| installed.generation().source_digest())
}

pub(super) async fn load_background_data(
    engine: &tokenx_engine::AcquisitionEngine,
    force: bool,
    last_digest: Option<u64>,
) -> Result<BackgroundLoad> {
    let mut prepared = engine.prepare()?;
    let digest = prepared.refresh_source_fingerprint().process_digest();
    if !force && last_digest == Some(digest) {
        return Ok(BackgroundLoad::Unchanged);
    }

    build_generation(engine, prepared)
        .await
        .map(|generation| BackgroundLoad::Loaded {
            generation: Box::new(generation),
            cache_persistence_warning: None,
        })
}

pub(super) fn persist_background_load(result: Result<BackgroundLoad>) -> Result<BackgroundLoad> {
    let result = result?;
    let BackgroundLoad::Loaded {
        generation,
        cache_persistence_warning: _,
    } = result
    else {
        return Ok(BackgroundLoad::Unchanged);
    };

    match save_generation_cache(&generation) {
        Ok(()) => Ok(BackgroundLoad::Loaded {
            generation,
            cache_persistence_warning: None,
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
            })
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{Tab, TuiConfig};
    use crate::tui::local_usage::LocalUsageStatus;

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
            failed_inputs: 1,
            complete: false,
            ..Default::default()
        };
        assert!(should_force_input_reload(false, Some(&degraded)));
        assert!(should_force_input_reload(true, None));
        assert!(!should_force_input_reload(false, None));
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
    fn installed_generation_is_the_source_digest_authority() {
        let (mut app, _) = harness(false);
        assert_eq!(installed_source_digest(&app), None);
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            tokenx_engine::InputFootprint::default(),
        );
        assert_eq!(
            installed_source_digest(&app),
            app.generation_for_test()
                .map(tokenx_engine::Generation::source_digest)
        );
    }
}
