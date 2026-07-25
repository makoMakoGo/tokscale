use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, HourlyViewMode, SortField, Tab};
use super::presentation::{Presentation, SubscriptionPresentation};
use super::view_state::ViewState;

/// A capability exposed by the current TUI view.
///
/// This is deliberately smaller than a command bus: it describes which
/// contextual commands may be advertised and dispatched, but does not execute
/// them or retain key-specific movement details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Scroll,
    PreviousTab,
    NextTab,
    Sort(SortField),
    OpenDetails,
    Back,
    ToggleView,
    Clients,
    GroupBy,
    Theme,
    ToggleAutoRefresh,
    IncreaseRefreshInterval,
    DecreaseRefreshInterval,
    RefreshLocal,
    RefreshSubscription,
    Copy,
    Export,
    Quit,
}

/// Ordered capabilities for one rendered view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActionSet {
    actions: Vec<Action>,
    empty_view: bool,
}

impl ActionSet {
    pub(crate) fn for_view(app: &App, state: &ViewState, presentation: Presentation) -> Self {
        if let Presentation::Subscription(subscription) = presentation {
            debug_assert_eq!(app.current_tab, Tab::Usage);
            return Self::for_usage(app, subscription);
        }
        debug_assert_ne!(app.current_tab, Tab::Usage);

        let installed = app.has_installed_generation();
        let empty = presentation.is_empty();

        if empty {
            debug_assert!(installed, "successful empty views require a generation");
            let mut actions = vec![Action::Clients];
            if !app.background_loading {
                actions.push(Action::RefreshLocal);
            }
            actions.extend([
                Action::PreviousTab,
                Action::NextTab,
                Action::Theme,
                Action::ToggleAutoRefresh,
                Action::IncreaseRefreshInterval,
                Action::DecreaseRefreshInterval,
                Action::Export,
                Action::Quit,
            ]);
            return Self {
                actions,
                empty_view: true,
            };
        }

        let mut actions = Vec::new();

        if installed && supports_scroll(app, state) {
            actions.push(Action::Scroll);
        }

        actions.extend([Action::PreviousTab, Action::NextTab]);

        if installed {
            actions.extend(sort_actions(app, state));
            actions.extend(view_actions(app, state));
        }

        if installed {
            actions.push(Action::Clients);
            if app.group_by_applies_to_current_tab() {
                actions.push(Action::GroupBy);
            }
        }

        actions.extend([Action::Theme, Action::ToggleAutoRefresh]);
        actions.extend([
            Action::IncreaseRefreshInterval,
            Action::DecreaseRefreshInterval,
        ]);
        if !app.background_loading {
            actions.push(Action::RefreshLocal);
        }
        if installed {
            actions.push(Action::Export);
            if supports_copy(app) {
                actions.push(Action::Copy);
            }
        }
        actions.push(Action::Quit);

        Self {
            actions,
            empty_view: empty,
        }
    }

    pub(crate) fn contains(&self, action: Action) -> bool {
        self.actions.contains(&action)
    }

    pub(crate) fn is_empty_view(&self) -> bool {
        self.empty_view
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = Action> + '_ {
        self.actions.iter().copied()
    }

    /// Classify keys that have a contextual capability without turning this
    /// module into the executor for those commands. Callers can consume a
    /// classified key when the returned action is absent from this set.
    pub(crate) fn action_for_key(app: &App, key: &KeyEvent) -> Option<Action> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Action::Quit);
        }

        match key.code {
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End => Some(Action::Scroll),
            KeyCode::Left | KeyCode::BackTab => Some(Action::PreviousTab),
            KeyCode::Right | KeyCode::Tab => Some(Action::NextTab),
            KeyCode::Char('d') => Some(Action::Sort(SortField::Date)),
            KeyCode::Char('t') => Some(Action::Sort(SortField::Tokens)),
            KeyCode::Char('c') => Some(Action::Sort(SortField::Cost)),
            KeyCode::Enter => Some(Action::OpenDetails),
            KeyCode::Esc | KeyCode::Backspace => Some(Action::Back),
            KeyCode::Char('h') if app.current_tab == Tab::Overview => Some(Action::ToggleView),
            KeyCode::Char('v') if matches!(app.current_tab, Tab::Daily | Tab::Hourly) => {
                Some(Action::ToggleView)
            }
            KeyCode::Char('s') => Some(Action::Clients),
            KeyCode::Char('g') => Some(Action::GroupBy),
            KeyCode::Char('p') => Some(Action::Theme),
            KeyCode::Char('R') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(Action::ToggleAutoRefresh)
            }
            KeyCode::Char('+') | KeyCode::Char('=') => Some(Action::IncreaseRefreshInterval),
            KeyCode::Char('-') => Some(Action::DecreaseRefreshInterval),
            KeyCode::Char('r') => Some(Action::RefreshLocal),
            KeyCode::Char('u') => Some(Action::RefreshSubscription),
            KeyCode::Char('y') => Some(Action::Copy),
            KeyCode::Char('e') => Some(Action::Export),
            KeyCode::Char('q') => Some(Action::Quit),
            _ => None,
        }
    }

    fn for_usage(app: &App, presentation: SubscriptionPresentation) -> Self {
        let mut actions = Vec::new();
        if matches!(presentation, SubscriptionPresentation::Results { .. }) {
            actions.push(Action::Scroll);
        }
        actions.extend([Action::PreviousTab, Action::NextTab, Action::Theme]);
        if app.has_enabled_subscription_providers() && !presentation.is_refreshing() {
            actions.push(Action::RefreshSubscription);
        }
        actions.push(Action::Quit);
        Self {
            actions,
            empty_view: false,
        }
    }
}

fn supports_copy(app: &App) -> bool {
    matches!(
        app.current_tab,
        Tab::Overview
            | Tab::Models
            | Tab::Monthly
            | Tab::Weekly
            | Tab::Daily
            | Tab::Hourly
            | Tab::Agents
    )
}

fn supports_scroll(app: &App, _state: &ViewState) -> bool {
    !matches!(app.current_tab, Tab::Overview | Tab::Stats)
}

fn sort_actions(app: &App, state: &ViewState) -> Vec<Action> {
    let fields: &[SortField] = match app.current_tab {
        Tab::Overview | Tab::Usage | Tab::Stats => &[],
        Tab::Daily if state.daily_profile_active() => &[],
        Tab::Hourly if app.hourly_view_mode == HourlyViewMode::Profile => &[],
        Tab::Sessions => &[SortField::Date, SortField::Tokens, SortField::Cost],
        Tab::Models | Tab::Monthly | Tab::Weekly | Tab::Daily | Tab::Hourly | Tab::Agents => {
            &[SortField::Date, SortField::Cost, SortField::Tokens]
        }
    };
    fields.iter().copied().map(Action::Sort).collect()
}

fn view_actions(app: &App, state: &ViewState) -> Vec<Action> {
    match app.current_tab {
        Tab::Models if app.is_model_detail_active() => vec![Action::Back],
        Tab::Models if app.model_details_supported() => vec![Action::OpenDetails],
        Tab::Models => Vec::new(),
        Tab::Monthly | Tab::Weekly if app.is_period_detail_active() => vec![Action::Back],
        Tab::Monthly | Tab::Weekly => vec![Action::OpenDetails],
        Tab::Daily if app.is_daily_detail_active() => vec![Action::Back],
        Tab::Daily if state.daily_profile_active() => vec![Action::ToggleView],
        Tab::Daily => vec![Action::OpenDetails, Action::ToggleView],
        Tab::Hourly => vec![Action::ToggleView],
        Tab::Sessions if state.session_detail_active() => vec![Action::Back],
        Tab::Sessions => state
            .selected_client_row(app)
            .is_some_and(|row| row.session_count > 0)
            .then_some(Action::OpenDetails)
            .into_iter()
            .collect(),
        Tab::Stats if app.selected_graph_cell.is_some() => vec![Action::Back],
        Tab::Overview => vec![Action::ToggleView],
        Tab::Usage | Tab::Stats | Tab::Agents => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::{NaiveDate, NaiveDateTime};
    use crossterm::event::KeyModifiers;
    use tokscale_core::{ClientId, GroupBy, InputFootprint, TuiAcc, TuiSessionEntry};

    use super::*;
    use crate::tui::app::{ProjectionBackend, SortDirection, TuiConfig};
    use crate::tui::data::{
        ContributionDay, ContributionGrade, DailyClientInfo, DailyModelInfo, DailyUsage, GraphData,
        HourlyModelInfo, HourlyUsage, TokenBreakdown,
    };
    use crate::tui::session_data::SessionSnapshot;
    use crate::tui::settings::Settings;

    fn make_app(tab: Tab, installed: bool) -> App {
        let config = TuiConfig {
            theme: Some("blue".to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: Some(tab),
        };
        let settings = Settings {
            usage_tab_enabled: tab == Tab::Usage,
            ..Settings::default()
        };
        let mut app = App::new_with_cached_data_and_settings(config, None, settings).unwrap();
        if installed {
            let accumulator = TuiAcc::default();
            let data = accumulator.project(&GroupBy::Model);
            app.install_tui_snapshot(
                data,
                Vec::new(),
                InputFootprint::default(),
                ProjectionBackend::Memory(accumulator),
                GroupBy::Model,
            );
        } else {
            app.data.error = Some("scan failed".to_string());
        }
        app
    }

    fn day() -> DailyUsage {
        let tokens = TokenBreakdown {
            input: 1,
            ..TokenBreakdown::default()
        };
        let model = DailyModelInfo {
            provider: "openai".to_string(),
            model_id: "gpt-5".to_string(),
            display_name: "gpt-5".to_string(),
            workspace_key: None,
            workspace_label: None,
            tokens: tokens.clone(),
            cost: 0.0,
            messages: 1,
        };
        DailyUsage {
            date: NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
            tokens: tokens.clone(),
            cost: 0.0,
            client_breakdown: BTreeMap::from([(
                "codex".to_string(),
                DailyClientInfo {
                    tokens,
                    cost: 0.0,
                    models: BTreeMap::from([("gpt-5".to_string(), model)]),
                },
            )]),
            message_count: 1,
            turn_count: 1,
        }
    }

    fn hourly() -> HourlyUsage {
        let tokens = TokenBreakdown {
            input: 1,
            ..TokenBreakdown::default()
        };
        HourlyUsage {
            datetime: NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
                chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            ),
            tokens: tokens.clone(),
            cost: 0.0,
            clients: BTreeSet::from(["codex".to_string()]),
            models: BTreeMap::from([(
                "gpt-5".to_string(),
                HourlyModelInfo {
                    provider: "openai".to_string(),
                    model_id: "gpt-5".to_string(),
                    display_name: "gpt-5".to_string(),
                    tokens,
                    cost: 0.0,
                },
            )]),
            message_count: 1,
            turn_count: 1,
        }
    }

    fn action_set(app: &App, state: &ViewState) -> ActionSet {
        let presentation = Presentation::for_view(app, state);
        ActionSet::for_view(app, state, presentation)
    }

    fn assert_installed_empty_actions(tab: Tab) {
        let app = make_app(tab, true);
        let set = action_set(&app, &ViewState::default());
        assert!(set.is_empty_view());
        for action in [
            Action::Clients,
            Action::RefreshLocal,
            Action::PreviousTab,
            Action::NextTab,
            Action::Theme,
            Action::ToggleAutoRefresh,
            Action::IncreaseRefreshInterval,
            Action::DecreaseRefreshInterval,
            Action::Export,
            Action::Quit,
        ] {
            assert!(set.contains(action), "missing {action:?}");
        }
        assert!(!set.contains(Action::Scroll));
        assert!(!set.contains(Action::OpenDetails));
        assert!(!set.iter().any(|action| matches!(action, Action::Sort(_))));
    }

    #[test]
    fn installed_empty_models_only_exposes_global_projection_actions() {
        assert_installed_empty_actions(Tab::Models);
    }

    #[test]
    fn installed_empty_daily_only_exposes_global_projection_actions() {
        assert_installed_empty_actions(Tab::Daily);
    }

    #[test]
    fn installed_empty_stats_has_no_scroll_or_sort() {
        assert_installed_empty_actions(Tab::Stats);
    }

    #[test]
    fn metadata_and_session_empty_views_keep_whole_report_export() {
        assert_installed_empty_actions(Tab::Agents);
        assert_installed_empty_actions(Tab::Sessions);
    }

    #[test]
    fn zero_session_client_row_cannot_open_detail() {
        let mut app = make_app(Tab::Sessions, true);
        app.session_snapshot = SessionSnapshot::new(
            Vec::new(),
            tokscale_core::InputFootprint::from_client_bytes([(ClientId::Codex, 0)]).unwrap(),
        );
        let set = action_set(&app, &ViewState::default());
        assert!(!set.is_empty_view());
        assert!(!set.contains(Action::OpenDetails));
        assert!(set.contains(Action::Sort(SortField::Date)));
    }

    #[test]
    fn cold_views_do_not_offer_projection_controls() {
        let app = make_app(Tab::Models, false);
        let set = action_set(&app, &ViewState::default());
        assert!(set.contains(Action::RefreshLocal));
        assert!(!set.contains(Action::Clients));
        assert!(!set.contains(Action::Export));

        let mut loading = make_app(Tab::Models, false);
        loading.background_loading = true;
        loading.data.error = None;
        let set = action_set(&loading, &ViewState::default());
        assert!(!set.contains(Action::RefreshLocal));
        assert!(!set.contains(Action::Clients));
        assert!(!set.contains(Action::GroupBy));
    }

    #[test]
    fn usage_actions_are_subscription_or_shell_scoped() {
        let mut app = make_app(Tab::Usage, true);
        app.set_subscription_provider_ids_for_test(vec![
            crate::tui::subscription_usage::UsageProviderId::Codex,
        ]);
        app.subscription_usage_errors = vec![crate::tui::subscription_usage::UsageProviderError {
            provider: "Claude".to_string(),
            message: "credential expired".to_string(),
        }];

        let set = action_set(&app, &ViewState::default());

        for action in [
            Action::Scroll,
            Action::PreviousTab,
            Action::NextTab,
            Action::Theme,
            Action::RefreshSubscription,
            Action::Quit,
        ] {
            assert!(set.contains(action), "missing {action:?}");
        }
        for action in [
            Action::RefreshLocal,
            Action::ToggleAutoRefresh,
            Action::IncreaseRefreshInterval,
            Action::DecreaseRefreshInterval,
            Action::Export,
            Action::Clients,
            Action::GroupBy,
        ] {
            assert!(!set.contains(action), "unexpected {action:?}");
        }
    }

    #[test]
    fn populated_daily_and_stats_expose_only_their_real_actions() {
        let mut daily = make_app(Tab::Daily, true);
        daily.data.daily.push(day());
        let daily_set = action_set(&daily, &ViewState::default());
        assert!(!daily_set.is_empty_view());
        assert!(daily_set.contains(Action::Scroll));
        assert!(daily_set.contains(Action::Sort(SortField::Date)));
        assert!(daily_set.contains(Action::OpenDetails));
        assert!(daily_set.contains(Action::Export));

        daily.data.graph = GraphData {
            weeks: vec![vec![Some(ContributionDay {
                date: NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
                tokens: 0,
                cost: 0.0,
                grade: ContributionGrade::Empty,
            })]],
        };
        daily.current_tab = Tab::Stats;
        let stats_set = action_set(&daily, &ViewState::default());
        assert!(!stats_set.contains(Action::Scroll));
        assert!(!stats_set
            .iter()
            .any(|action| matches!(action, Action::Sort(_))));
    }

    #[test]
    fn hourly_profile_keeps_scroll_and_toggle_but_not_table_sort() {
        let mut app = make_app(Tab::Hourly, true);
        app.data.hourly.push(hourly());
        let table = action_set(&app, &ViewState::default());
        assert!(table.contains(Action::Sort(SortField::Date)));

        app.hourly_view_mode = HourlyViewMode::Profile;
        let profile = action_set(&app, &ViewState::default());
        assert!(profile.contains(Action::Scroll));
        assert!(profile.contains(Action::ToggleView));
        assert!(!profile
            .iter()
            .any(|action| matches!(action, Action::Sort(_))));
    }

    #[test]
    fn sessions_with_rows_exposes_list_actions() {
        let mut app = make_app(Tab::Sessions, true);
        app.session_snapshot = SessionSnapshot::new(
            vec![TuiSessionEntry {
                client: "codex".to_string(),
                session_id: "session-1".to_string(),
                ..TuiSessionEntry::default()
            }],
            tokscale_core::InputFootprint::default(),
        );
        let set = action_set(&app, &ViewState::default());

        assert!(set.contains(Action::Scroll));
        assert!(set.contains(Action::Sort(SortField::Tokens)));
        assert!(set.contains(Action::OpenDetails));
        assert!(set.contains(Action::Export));
    }

    #[test]
    fn session_detail_action_follows_the_selected_row_in_sort_order() {
        let mut app = make_app(Tab::Sessions, true);
        app.session_snapshot = SessionSnapshot::new(
            vec![TuiSessionEntry {
                client: "claude".to_string(),
                session_id: "session-1".to_string(),
                ..TuiSessionEntry::default()
            }],
            tokscale_core::InputFootprint::from_client_bytes([(ClientId::Codex, 0)]).unwrap(),
        );
        app.sort_field = SortField::Tokens;
        app.sort_direction = SortDirection::Ascending;

        let mut state = ViewState::default();
        assert!(!action_set(&app, &state).contains(Action::OpenDetails));
        assert!(state.handle_key(&app, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert!(action_set(&app, &state).contains(Action::OpenDetails));

        app.sort_direction = SortDirection::Descending;
        let state = ViewState::default();
        assert!(action_set(&app, &state).contains(Action::OpenDetails));
    }

    #[test]
    fn key_classification_distinguishes_commands_and_ctrl_c() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            ActionSet::action_for_key(&make_app(Tab::Daily, true), &key(KeyCode::Char('d'))),
            Some(Action::Sort(SortField::Date))
        );
        assert_eq!(
            ActionSet::action_for_key(&make_app(Tab::Models, true), &key(KeyCode::Enter)),
            Some(Action::OpenDetails)
        );
        assert_eq!(
            ActionSet::action_for_key(&make_app(Tab::Models, true), &key(KeyCode::Left)),
            Some(Action::PreviousTab)
        );
        assert_eq!(
            ActionSet::action_for_key(
                &make_app(Tab::Models, true),
                &KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT,),
            ),
            Some(Action::ToggleAutoRefresh)
        );
        assert_eq!(
            ActionSet::action_for_key(
                &make_app(Tab::Models, true),
                &KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL,),
            ),
            Some(Action::Quit)
        );
        assert_eq!(
            ActionSet::action_for_key(&make_app(Tab::Models, true), &key(KeyCode::Char('y'))),
            Some(Action::Copy)
        );
        assert_eq!(
            ActionSet::action_for_key(&make_app(Tab::Models, true), &key(KeyCode::Char('+'))),
            Some(Action::IncreaseRefreshInterval)
        );
        assert_eq!(
            ActionSet::action_for_key(&make_app(Tab::Usage, true), &key(KeyCode::Char('u'))),
            Some(Action::RefreshSubscription)
        );

        let overview = make_app(Tab::Overview, true);
        let daily = make_app(Tab::Daily, true);
        let models = make_app(Tab::Models, true);
        assert_eq!(
            ActionSet::action_for_key(&overview, &key(KeyCode::Char('h'))),
            Some(Action::ToggleView)
        );
        assert_eq!(
            ActionSet::action_for_key(&daily, &key(KeyCode::Char('v'))),
            Some(Action::ToggleView)
        );
        assert_eq!(
            ActionSet::action_for_key(&models, &key(KeyCode::Char('h'))),
            None
        );
        assert_eq!(
            ActionSet::action_for_key(&overview, &key(KeyCode::Char('v'))),
            None
        );
        assert_eq!(
            ActionSet::action_for_key(&daily, &key(KeyCode::Char('j'))),
            None
        );
    }
}
