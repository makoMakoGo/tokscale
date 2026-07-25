use super::app::{App, ChartGranularity, Tab};
use super::data::PeriodKind;
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
        let refreshing = app.is_fetching_usage();
        let has_snapshot =
            !app.subscription_usage.is_empty() || !app.subscription_usage_errors.is_empty();
        if refreshing && !has_snapshot {
            return Self::ColdFetching;
        }

        let has_results = !app.subscription_usage_errors.is_empty()
            || app
                .subscription_usage
                .iter()
                .any(|output| !output.metrics.is_empty());
        if has_results {
            return Self::Results { refreshing };
        }

        if app.usage_fetch_attempted || !app.subscription_usage.is_empty() {
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
    /// Usage tab owns a separate presentation authority nested in this route.
    pub(crate) fn for_view(app: &App, state: &ViewState) -> Self {
        if !app.current_tab.depends_on_local_generation() {
            return Self::Subscription(SubscriptionPresentation::for_app(app));
        }

        if !app.has_installed_generation() {
            return if app.background_loading {
                Self::Loading
            } else {
                Self::Failed
            };
        }

        empty_subject(app, state).map_or(Self::Ready, Self::Empty)
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

fn empty_subject(app: &App, state: &ViewState) -> Option<EmptySubject> {
    use EmptySubject::{AgentBreakdown, Sessions, Usage};

    let subject = match app.current_tab {
        Tab::Overview => match app.chart_granularity {
            ChartGranularity::Daily if app.data.daily.is_empty() => Usage,
            ChartGranularity::Hourly if app.data.hourly.is_empty() => Usage,
            ChartGranularity::Daily | ChartGranularity::Hourly => return None,
        },
        Tab::Models if !app.is_model_detail_active() && app.data.models.is_empty() => Usage,
        Tab::Agents if app.data.agents.is_empty() => AgentBreakdown,
        Tab::Daily if !app.is_daily_detail_active() && app.data.daily.is_empty() => Usage,
        Tab::Hourly if app.data.hourly.is_empty() => Usage,
        Tab::Monthly
            if !app.is_period_detail_active_for_kind(PeriodKind::Monthly)
                && app.data.daily.is_empty() =>
        {
            Usage
        }
        Tab::Weekly
            if !app.is_period_detail_active_for_kind(PeriodKind::Weekly)
                && app.data.daily.is_empty() =>
        {
            Usage
        }
        Tab::Stats if app.data.graph.weeks.is_empty() => Usage,
        Tab::Sessions if !state.session_detail_active() && state.client_count(app) == 0 => Sessions,
        Tab::Usage => unreachable!("remote Usage does not use local presentation state"),
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
    use crate::tui::app::TuiConfig;
    use crate::tui::settings::Settings;
    use tokscale_core::{
        build_usage_index, ClientId, DateRange, TokenBreakdown, UnifiedMessage, UsageIndex,
    };

    fn app(tab: Tab, installed: bool) -> App {
        let settings = Settings {
            usage_tab_enabled: tab == Tab::Usage,
            ..Settings::default()
        };
        let mut app = App::new_with_cached_data_and_settings(
            TuiConfig {
                theme: Some("blue".to_string()),
                refresh: 0,
                no_refresh: false,
                home_dir: None,
                client_universe: tokscale_core::ClientUniverse::all(),
                since: None,
                until: None,
                year: None,
                initial_tab: Some(tab),
            },
            None,
            settings,
        )
        .expect("test app initializes");

        if installed {
            install_generation(&mut app, UsageIndex::default());
        }
        app
    }

    fn install_generation(app: &mut App, accumulator: UsageIndex) {
        app.install_generation_fixture(accumulator, Vec::new(), Default::default());
    }

    fn populated_accumulator() -> UsageIndex {
        build_usage_index(
            &[UnifiedMessage::new(
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

        cold.background_loading = true;
        assert_eq!(Presentation::for_view(&cold, &state), Presentation::Loading);

        cold.background_loading = false;
        cold.data.error = Some("scan failed".to_string());
        assert_eq!(Presentation::for_view(&cold, &state), Presentation::Failed);

        let installed = app(Tab::Models, true);
        assert_eq!(
            Presentation::for_view(&installed, &state),
            Presentation::Empty(EmptySubject::Usage)
        );
    }

    #[test]
    fn subscription_prompt_does_not_inherit_local_acquisition_state() {
        let mut app = app(Tab::Usage, false);
        app.background_loading = true;

        assert_eq!(
            Presentation::for_view(&app, &ViewState::default()),
            Presentation::Subscription(SubscriptionPresentation::Prompt)
        );
    }

    #[test]
    fn subscription_presentation_classifies_its_own_lifecycle() {
        let mut prompt = app(Tab::Usage, false);
        assert_eq!(
            SubscriptionPresentation::for_app(&prompt),
            SubscriptionPresentation::Prompt
        );

        prompt.usage_fetch_attempted = true;
        assert_eq!(
            SubscriptionPresentation::for_app(&prompt),
            SubscriptionPresentation::Empty { refreshing: false }
        );

        let mut cold_fetch = app(Tab::Usage, false);
        let (_tx, rx) = std::sync::mpsc::channel();
        cold_fetch.start_subscription_usage_fetch_for_test(rx);
        assert_eq!(
            SubscriptionPresentation::for_app(&cold_fetch),
            SubscriptionPresentation::ColdFetching
        );

        let mut results = app(Tab::Usage, false);
        results.subscription_usage_errors =
            vec![crate::tui::subscription_usage::UsageProviderError {
                provider: "Codex".to_string(),
                message: "credential expired".to_string(),
            }];
        assert_eq!(
            SubscriptionPresentation::for_app(&results),
            SubscriptionPresentation::Results { refreshing: false }
        );

        let (_tx, rx) = std::sync::mpsc::channel();
        results.start_subscription_usage_fetch_for_test(rx);
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
