use super::app::{App, ChartGranularity, Tab};
use super::data::PeriodKind;
use super::local_usage::{InstalledGeneration, LocalUsageStatus};
use super::view_state::ViewState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmptySubject {
    Usage,
    AgentBreakdown,
    Sessions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presentation {
    Loading,
    Failed,
    Empty(EmptySubject),
    Ready,
    Subscription(SubscriptionPresentation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubscriptionPresentation {
    ColdFetching,
    Prompt,
    Empty { refreshing: bool },
    Results { refreshing: bool },
}

impl SubscriptionPresentation {
    pub(crate) fn for_app(app: &App) -> Self {
        let refreshing = app.is_fetching_subscription();
        let has_snapshot =
            !app.subscription_outputs().is_empty() || !app.subscription_errors().is_empty();
        if refreshing && !has_snapshot {
            return Self::ColdFetching;
        }

        let has_results = !app.subscription_errors().is_empty()
            || app
                .subscription_outputs()
                .iter()
                .any(|output| !output.metrics.is_empty());
        if has_results {
            return Self::Results { refreshing };
        }

        if app.has_subscription_fetch_history() || !app.subscription_outputs().is_empty() {
            Self::Empty { refreshing }
        } else {
            Self::Prompt
        }
    }

    pub(crate) const fn is_refreshing(self) -> bool {
        match self {
            Self::ColdFetching => true,
            Self::Prompt => false,
            Self::Empty { refreshing } | Self::Results { refreshing } => refreshing,
        }
    }
}

impl Presentation {
    /// Classify the state users can actually reach for the current view.
    ///
    /// Local tabs have one acquisition boundary: before the first generation
    /// they are loading or failed; afterwards they render the installed
    /// generation, which may be empty for the current projection. The remote
    /// Subscription tab owns a separate presentation authority nested in this route.
    pub(crate) fn for_view(app: &App, state: &ViewState) -> Self {
        if !app.current_tab.depends_on_local_generation() {
            return Self::Subscription(SubscriptionPresentation::for_app(app));
        }

        if app.is_cold_loading() {
            return Self::Loading;
        }

        match app.local_usage_status() {
            LocalUsageStatus::Empty | LocalUsageStatus::Failed { .. } => return Self::Failed,
            LocalUsageStatus::Ready | LocalUsageStatus::Degraded { .. } => {}
        }

        let installed = app
            .installed_generation()
            .expect("readable local lifecycle state must carry an installed generation");
        empty_subject(app, state, installed).map_or(Self::Ready, Self::Empty)
    }

    pub(crate) fn empty_subject(self) -> Option<EmptySubject> {
        match self {
            Self::Empty(subject) => Some(subject),
            Self::Loading | Self::Failed | Self::Ready | Self::Subscription(_) => None,
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        matches!(self, Self::Empty(_))
    }
}

fn empty_subject(
    app: &App,
    state: &ViewState,
    installed: &InstalledGeneration,
) -> Option<EmptySubject> {
    use EmptySubject::{AgentBreakdown, Sessions, Usage};

    let usage = installed.view();
    let subject = match app.current_tab {
        Tab::Overview => match app.chart_granularity {
            ChartGranularity::Daily if usage.daily.is_empty() => Usage,
            ChartGranularity::Hourly if usage.hourly.is_empty() => Usage,
            ChartGranularity::Daily | ChartGranularity::Hourly => return None,
        },
        Tab::Models if !app.is_model_detail_active() && usage.models.is_empty() => Usage,
        Tab::Agents if usage.agents.is_empty() => AgentBreakdown,
        Tab::Daily if !app.is_daily_detail_active() && usage.daily.is_empty() => Usage,
        Tab::Hourly if usage.hourly.is_empty() => Usage,
        Tab::Monthly
            if !app.is_period_detail_active_for_kind(PeriodKind::Monthly)
                && usage.daily.is_empty() =>
        {
            Usage
        }
        Tab::Weekly
            if !app.is_period_detail_active_for_kind(PeriodKind::Weekly)
                && usage.daily.is_empty() =>
        {
            Usage
        }
        Tab::Stats if usage.graph.weeks.is_empty() => Usage,
        Tab::Sessions
            if !state.session_detail_active()
                && installed
                    .sessions()
                    .client_summaries()
                    .iter()
                    .all(|summary| !app.is_client_selected(summary.client)) =>
        {
            Sessions
        }
        Tab::Subscription => {
            unreachable!("Subscription does not use local presentation state")
        }
        Tab::Models
        | Tab::Monthly
        | Tab::Weekly
        | Tab::Daily
        | Tab::Hourly
        | Tab::Stats
        | Tab::Agents
        | Tab::Sessions => return None,
    };

    Some(subject)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::tui::app::TuiConfig;
    use tokenx_engine::{
        build_usage_index, AttributedUsageRecord, ClientId, DateRange, FrozenUsageIndex,
        TokenBreakdown,
    };

    fn app(tab: Tab, installed: bool) -> App {
        let mut settings = Settings::default();
        settings.subscription.enabled = tab == Tab::Subscription;
        let mut app = App::new_for_test_with_settings(
            TuiConfig {
                theme: Some(crate::theme::ThemeName::Blue),
                refresh: 0,
                no_refresh: false,
                client_universe: tokenx_engine::ClientUniverse::all(),
                initial_tab: Some(tab),
                effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            },
            settings,
        )
        .expect("test app initializes");

        if installed {
            install_generation(&mut app, FrozenUsageIndex::default());
        }
        app
    }

    fn install_generation(app: &mut App, accumulator: FrozenUsageIndex) {
        app.install_generation_fixture(accumulator, Vec::new(), Default::default());
    }

    fn populated_accumulator() -> FrozenUsageIndex {
        build_usage_index(
            &[AttributedUsageRecord::new(
                ClientId::Codex,
                "gpt-5",
                "openai",
                "session-1",
                1_700_000_000_000,
                TokenBreakdown {
                    input: 1,
                    ..TokenBreakdown::default()
                },
                0.0,
            )],
            DateRange::none(),
        )
    }

    #[test]
    fn local_lifecycle_has_no_constructor_only_ready_state() {
        let state = ViewState::default();
        let mut cold = app(Tab::Models, false);

        cold.set_refresh_loading_for_test(true);
        assert_eq!(Presentation::for_view(&cold, &state), Presentation::Loading);

        cold.fail_refresh_for_test("scan failed".to_string());
        assert_eq!(Presentation::for_view(&cold, &state), Presentation::Failed);

        let installed = app(Tab::Models, true);
        assert_eq!(
            Presentation::for_view(&installed, &state),
            Presentation::Empty(EmptySubject::Usage)
        );
    }

    #[test]
    fn subscription_prompt_does_not_inherit_local_acquisition_state() {
        let mut app = app(Tab::Subscription, false);
        app.set_refresh_loading_for_test(true);

        assert_eq!(
            Presentation::for_view(&app, &ViewState::default()),
            Presentation::Subscription(SubscriptionPresentation::Prompt)
        );
    }

    #[test]
    fn subscription_presentation_classifies_its_own_lifecycle() {
        let mut prompt = app(Tab::Subscription, false);
        assert_eq!(
            SubscriptionPresentation::for_app(&prompt),
            SubscriptionPresentation::Prompt
        );

        prompt.set_subscription_provider_ids_for_test(vec![crate::subscription::ProviderId::Codex]);
        prompt.fetch_subscription();
        let request = prompt
            .take_subscription_request()
            .expect("fetch request is queued");
        request.1.send(Default::default()).unwrap();
        prompt.on_tick();
        assert_eq!(
            SubscriptionPresentation::for_app(&prompt),
            SubscriptionPresentation::Empty { refreshing: false }
        );

        let mut cold_fetch = app(Tab::Subscription, false);
        let (_tx, rx) = std::sync::mpsc::channel();
        cold_fetch.start_subscription_fetch_for_test(rx);
        assert_eq!(
            SubscriptionPresentation::for_app(&cold_fetch),
            SubscriptionPresentation::ColdFetching
        );

        let mut results = app(Tab::Subscription, false);
        results.replace_subscription_errors_for_test(vec![
            crate::subscription::SubscriptionError {
                provider: "Codex".to_string(),
                message: "credential expired".to_string(),
            },
        ]);
        assert_eq!(
            SubscriptionPresentation::for_app(&results),
            SubscriptionPresentation::Results { refreshing: false }
        );

        let (_tx, rx) = std::sync::mpsc::channel();
        results.start_subscription_fetch_for_test(rx);
        assert_eq!(
            SubscriptionPresentation::for_app(&results),
            SubscriptionPresentation::Results { refreshing: true }
        );
    }

    #[test]
    fn period_roots_classify_empty_state_from_daily_structure() {
        let state = ViewState::default();

        for tab in [Tab::Monthly, Tab::Weekly] {
            let empty = app(tab, true);
            assert_eq!(
                Presentation::for_view(&empty, &state),
                Presentation::Empty(EmptySubject::Usage)
            );

            let mut populated = app(tab, false);
            install_generation(&mut populated, populated_accumulator());
            assert_eq!(
                Presentation::for_view(&populated, &state),
                Presentation::Ready
            );
        }
    }
}
