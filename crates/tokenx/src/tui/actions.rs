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
            debug_assert_eq!(app.current_tab, Tab::Subscription);
            return Self::for_subscription(app, subscription);
        }
        debug_assert_ne!(app.current_tab, Tab::Subscription);

        let installed = app.has_installed_generation();
        let empty = presentation.is_empty();

        if empty {
            debug_assert!(installed, "successful empty views require a generation");
            let mut actions = vec![Action::Clients];
            if !app.is_background_loading() {
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
        if !app.is_background_loading() {
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

    fn for_subscription(app: &App, presentation: SubscriptionPresentation) -> Self {
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
        Tab::Overview | Tab::Subscription | Tab::Stats => &[],
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
        Tab::Subscription | Tab::Stats | Tab::Agents => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::{NaiveDate, NaiveDateTime};
    use crossterm::event::KeyModifiers;
    use tokenx_engine::{ClientId, FrozenUsageIndex, InputFootprint, SessionUsage};

    use super::*;
    use crate::settings::Settings;
    use crate::tui::app::{SortDirection, TuiConfig};
    use crate::tui::data::{
        ContributionDay, ContributionGrade, DailyClientInfo, DailyModelInfo, DailyUsage,
        HourlyModelInfo, HourlyUsage, UsageGraphData, UsageTokenBreakdown,
    };
    use crate::tui::session_data::SessionSnapshot;

    fn make_app(tab: Tab, installed: bool) -> App {
        let config = TuiConfig {
            theme: Some(crate::theme::ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: Some(tab),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let mut settings = Settings::default();
        settings.subscription.enabled = tab == Tab::Subscription;
        let mut app = App::new_for_test_with_settings(config, settings).unwrap();
        if installed {
            app.install_generation_fixture(
                FrozenUsageIndex::default(),
                Vec::new(),
                InputFootprint::default(),
            );
        } else {
            app.set_refresh_loading_for_test(true);
            app.fail_refresh_for_test("scan failed".to_string());
        }
        app
    }

    fn day() -> DailyUsage {
        let tokens = UsageTokenBreakdown {
            input: 1,
            ..UsageTokenBreakdown::default()
        };
        let model = DailyModelInfo {
            provider: "openai".into(),
            model_id: "gpt-5".into(),
            display_name: "gpt-5".into(),
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
                ClientId::Codex,
                DailyClientInfo {
                    tokens,
                    cost: 0.0,
                    models: vec![model],
                },
            )]),
            message_count: 1,
            turn_count: 1,
        }
    }

    fn hourly() -> HourlyUsage {
        let tokens = UsageTokenBreakdown {
            input: 1,
            ..UsageTokenBreakdown::default()
        };
        HourlyUsage {
            datetime: NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
                chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            ),
            tokens: tokens.clone(),
            cost: 0.0,
            clients: BTreeSet::from([ClientId::Codex]),
            models: vec![HourlyModelInfo {
                provider: "openai".into(),
                model_id: "gpt-5".into(),
                display_name: "gpt-5".into(),
                tokens,
                cost: 0.0,
            }],
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
    fn empty_agent_metadata_keeps_generation_export() {
        assert_installed_empty_actions(Tab::Agents);
    }

    #[test]
    fn zero_session_client_row_cannot_open_detail() {
        let mut app = make_app(Tab::Sessions, true);
        app.replace_session_snapshot_for_test(SessionSnapshot::new(
            Vec::new(),
            &tokenx_engine::InputFootprint::from_client_bytes([(ClientId::Codex, 0)]).unwrap(),
        ));
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
        loading.set_refresh_loading_for_test(true);
        let set = action_set(&loading, &ViewState::default());
        assert!(!set.contains(Action::RefreshLocal));
        assert!(!set.contains(Action::Clients));
        assert!(!set.contains(Action::GroupBy));
    }

    #[test]
    fn subscription_actions_are_subscription_or_shell_scoped() {
        let mut app = make_app(Tab::Subscription, true);
        app.set_subscription_provider_ids_for_test(vec![crate::subscription::ProviderId::Codex]);
        app.replace_subscription_errors_for_test(vec![crate::subscription::SubscriptionError {
            provider_id: Some(crate::subscription::ProviderId::Claude),
            provider: "Claude".to_string(),
            message: "credential expired".to_string(),
        }]);

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
        daily.usage_mut_for_test().daily.push(day());
        let daily_set = action_set(&daily, &ViewState::default());
        assert!(!daily_set.is_empty_view());
        assert!(daily_set.contains(Action::Scroll));
        assert!(daily_set.contains(Action::Sort(SortField::Date)));
        assert!(daily_set.contains(Action::OpenDetails));
        assert!(daily_set.contains(Action::Export));

        daily.usage_mut_for_test().graph = UsageGraphData {
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
        app.usage_mut_for_test().hourly.push(hourly());
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
        app.replace_session_snapshot_for_test(SessionSnapshot::new(
            vec![SessionUsage::new(ClientId::Codex, "session-1")],
            &tokenx_engine::InputFootprint::default(),
        ));
        let set = action_set(&app, &ViewState::default());

        assert!(set.contains(Action::Scroll));
        assert!(set.contains(Action::Sort(SortField::Tokens)));
        assert!(set.contains(Action::OpenDetails));
        assert!(set.contains(Action::Export));
    }

    #[test]
    fn session_detail_action_follows_the_selected_row_in_sort_order() {
        let mut app = make_app(Tab::Sessions, true);
        app.replace_session_snapshot_for_test(SessionSnapshot::new(
            vec![SessionUsage::new(ClientId::Claude, "session-1")],
            &tokenx_engine::InputFootprint::from_client_bytes([(ClientId::Codex, 0)]).unwrap(),
        ));
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
            ActionSet::action_for_key(&make_app(Tab::Subscription, true), &key(KeyCode::Char('u'))),
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
