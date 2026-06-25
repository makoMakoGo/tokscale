use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::commands::usage::UsageProviderId;
use anyhow::Result;
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use tokscale_core::{ordered_clients_by_token_contribution, ClientContributionOrder, ClientId};

use ratatui::style::Color;

use super::data::{
    build_period_usage, AgentUsage, DailySourceInfo, DailyUsage, DataLoader, HourlyUsage,
    ModelUsage, PeriodKind, PeriodUsage, TokenBreakdown, UsageData,
};
use super::interaction::{
    InteractionOutcome, ListInteraction, MoveCommand, TextViewport, WrapMode,
};
use super::settings::Settings;
use super::themes::{Theme, ThemeName};
use super::ui::dialog::{ClientPickerDialog, DialogStack};
use super::ui::widgets::{get_model_color, get_provider_shade, provider_color_key};

/// Configuration for TUI initialization
pub struct TuiConfig {
    pub theme: String,
    pub refresh: u64,
    pub sessions_path: Option<String>,
    pub clients: Option<Vec<String>>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
    pub initial_tab: Option<Tab>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Overview,
    Usage,
    Models,
    Monthly,
    Weekly,
    Daily,
    Hourly,
    Stats,
    Agents,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[
            Tab::Overview,
            Tab::Usage,
            Tab::Models,
            Tab::Monthly,
            Tab::Weekly,
            Tab::Daily,
            Tab::Hourly,
            Tab::Stats,
            Tab::Agents,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Usage => "Usage",
            Tab::Models => "Models",
            Tab::Monthly => "Monthly",
            Tab::Weekly => "Weekly",
            Tab::Daily => "Daily",
            Tab::Hourly => "Hourly",
            Tab::Stats => "Stats",
            Tab::Agents => "Agents",
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Tab::Overview => "Ovw",
            Tab::Usage => "Use",
            Tab::Models => "Mod",
            Tab::Monthly => "Mth",
            Tab::Weekly => "Wk",
            Tab::Daily => "Day",
            Tab::Hourly => "Hr",
            Tab::Stats => "Sta",
            Tab::Agents => "Agt",
        }
    }

    pub fn next(self) -> Tab {
        match self {
            Tab::Overview => Tab::Usage,
            Tab::Usage => Tab::Models,
            Tab::Models => Tab::Monthly,
            Tab::Monthly => Tab::Weekly,
            Tab::Weekly => Tab::Daily,
            Tab::Daily => Tab::Hourly,
            Tab::Hourly => Tab::Stats,
            Tab::Stats => Tab::Agents,
            Tab::Agents => Tab::Overview,
        }
    }

    pub fn prev(self) -> Tab {
        match self {
            Tab::Overview => Tab::Agents,
            Tab::Usage => Tab::Overview,
            Tab::Models => Tab::Usage,
            Tab::Monthly => Tab::Models,
            Tab::Weekly => Tab::Monthly,
            Tab::Daily => Tab::Weekly,
            Tab::Hourly => Tab::Daily,
            Tab::Stats => Tab::Hourly,
            Tab::Agents => Tab::Stats,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChartGranularity {
    #[default]
    Daily,
    Hourly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Cost,
    Tokens,
    Date,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HourlyViewMode {
    #[default]
    Table,
    Profile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

pub struct ClickArea {
    pub rect: Rect,
    pub action: ClickAction,
}

#[derive(Debug, Clone)]
pub struct DetailRow {
    pub source: String,
    pub provider: String,
    pub model: String,
    pub color_key: String,
    pub tokens: TokenBreakdown,
    pub cost: f64,
    pub messages: u64,
}

pub type DailyDetailRow = DetailRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodDetailSelection {
    pub kind: PeriodKind,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

pub type PeriodDetailRow = DetailRow;

#[derive(Debug, Clone)]
pub enum ClickAction {
    Tab(Tab),
    Sort(SortField),
    GraphCell { week: usize, day: usize },
}

struct DetailRowAccumulator {
    source_totals: HashMap<String, ClientContributionOrder>,
    provider: String,
    model: String,
    color_key: String,
    tokens: TokenBreakdown,
    cost: f64,
    messages: u64,
}

fn move_command_from_key(key: KeyCode) -> Option<MoveCommand> {
    match key {
        KeyCode::Up => Some(MoveCommand::Up),
        KeyCode::Down => Some(MoveCommand::Down),
        KeyCode::PageUp => Some(MoveCommand::PageUp),
        KeyCode::PageDown => Some(MoveCommand::PageDown),
        KeyCode::Home => Some(MoveCommand::Home),
        KeyCode::End => Some(MoveCommand::End),
        _ => None,
    }
}

fn add_detail_tokens(target: &mut TokenBreakdown, source: &TokenBreakdown) {
    target.input = target.input.saturating_add(source.input);
    target.output = target.output.saturating_add(source.output);
    target.cache_read = target.cache_read.saturating_add(source.cache_read);
    target.cache_write = target.cache_write.saturating_add(source.cache_write);
    target.reasoning = target.reasoning.saturating_add(source.reasoning);
}

fn merge_provider_label(target: &mut String, provider: &str) {
    if provider.is_empty() || target.split(", ").any(|existing| existing == provider) {
        return;
    }
    if target.is_empty() {
        target.push_str(provider);
    } else {
        target.push_str(", ");
        target.push_str(provider);
    }
}

fn build_detail_rows(source_breakdown: &BTreeMap<String, DailySourceInfo>) -> Vec<DetailRow> {
    let mut rows_by_key: BTreeMap<String, DetailRowAccumulator> = BTreeMap::new();

    for (source, source_info) in source_breakdown {
        for (model_key, model_info) in &source_info.models {
            let row =
                rows_by_key
                    .entry(model_key.clone())
                    .or_insert_with(|| DetailRowAccumulator {
                        source_totals: HashMap::new(),
                        provider: String::new(),
                        model: if model_info.display_name.is_empty() {
                            model_key.clone()
                        } else {
                            model_info.display_name.clone()
                        },
                        // Merged detail buckets share a model-derived color key.
                        color_key: model_info.color_key.clone(),
                        tokens: TokenBreakdown::default(),
                        cost: 0.0,
                        messages: 0,
                    });

            let source_count = row.source_totals.len();
            let source_total = row.source_totals.entry(source.clone()).or_insert_with(|| {
                ClientContributionOrder {
                    first_seen: source_count,
                    total_tokens: 0,
                }
            });
            source_total.total_tokens = source_total
                .total_tokens
                .saturating_add(model_info.tokens.total());

            merge_provider_label(&mut row.provider, &model_info.provider);
            add_detail_tokens(&mut row.tokens, &model_info.tokens);
            row.cost += model_info.cost;
            row.messages = row.messages.saturating_add(model_info.messages);
        }
    }

    rows_by_key
        .into_values()
        .map(|row| DetailRow {
            source: ordered_clients_by_token_contribution(&row.source_totals),
            provider: row.provider,
            model: row.model,
            color_key: row.color_key,
            tokens: row.tokens,
            cost: row.cost,
            messages: row.messages,
        })
        .collect()
}

fn sort_detail_rows(rows: &mut [DetailRow], field: SortField, direction: SortDirection) {
    let tie_breaker = |a: &DetailRow, b: &DetailRow| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.provider.cmp(&b.provider))
    };

    match (field, direction) {
        (SortField::Cost, SortDirection::Descending) => {
            rows.sort_by(|a, b| b.cost.total_cmp(&a.cost).then_with(|| tie_breaker(a, b)))
        }
        (SortField::Cost, SortDirection::Ascending) => {
            rows.sort_by(|a, b| a.cost.total_cmp(&b.cost).then_with(|| tie_breaker(a, b)))
        }
        (SortField::Tokens, SortDirection::Descending) => rows.sort_by(|a, b| {
            b.tokens
                .total()
                .cmp(&a.tokens.total())
                .then_with(|| tie_breaker(a, b))
        }),
        (SortField::Tokens, SortDirection::Ascending) => rows.sort_by(|a, b| {
            a.tokens
                .total()
                .cmp(&b.tokens.total())
                .then_with(|| tie_breaker(a, b))
        }),
        (SortField::Date, _) => rows.sort_by(tie_breaker),
    }
}

pub struct App {
    pub should_quit: bool,
    pub current_tab: Tab,
    pub theme: Theme,
    pub settings: Settings,
    pub data: UsageData,
    pub data_loader: DataLoader,

    /// Set of clients currently selected in the source picker.
    pub enabled_clients: Rc<RefCell<HashSet<ClientId>>>,
    pub group_by: Rc<RefCell<tokscale_core::GroupBy>>,
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
    tab_sort_state: HashMap<Tab, (SortField, SortDirection)>,
    pub chart_granularity: ChartGranularity,

    pub scroll_offset: usize,
    pub selected_index: usize,
    pub max_visible_items: usize,
    pub(crate) usage_viewport: TextViewport,
    usage_text_total_lines: usize,
    pub(crate) hourly_profile_viewport: TextViewport,
    hourly_profile_text_total_lines: usize,
    pub selected_daily_detail_date: Option<NaiveDate>,
    daily_list_selected_index: usize,
    daily_list_scroll_offset: usize,
    daily_list_sort_before_detail: Option<(SortField, SortDirection)>,
    daily_detail_sort_state: Option<(SortField, SortDirection)>,
    pub selected_period_detail: Option<PeriodDetailSelection>,
    period_list_selected_index: usize,
    period_list_scroll_offset: usize,
    period_list_sort_before_detail: Option<(SortField, SortDirection)>,
    period_detail_sort_state: Option<(SortField, SortDirection)>,

    pub selected_graph_cell: Option<(usize, usize)>,
    pub stats_breakdown_total_lines: usize,

    pub auto_refresh: bool,
    pub auto_refresh_interval: Duration,
    pub last_refresh: Instant,
    pub last_subscription_usage_check: Option<Instant>,

    pub status_message: Option<String>,
    pub status_message_time: Option<Instant>,

    pub terminal_width: u16,
    pub terminal_height: u16,

    pub click_areas: Vec<ClickArea>,

    pub spinner_frame: usize,

    pub background_loading: bool,

    pub blocking_loading: bool,

    pub needs_reload: bool,

    /// Forces the next background reload to skip the source-digest probe
    /// (manual refresh and filter changes must always re-aggregate).
    pub reload_force: bool,

    /// Digest of the scanned sources at the last completed load; auto-refresh
    /// skips the parse when a fresh probe matches (ADR 0008).
    pub last_source_digest: Option<u64>,

    pub dialog_stack: DialogStack,

    pub dialog_needs_reload: Rc<RefCell<bool>>,

    pub hourly_view_mode: HourlyViewMode,

    pub model_shade_map: HashMap<String, Color>,

    pub subscription_usage: Vec<crate::commands::usage::UsageOutput>,
    pub subscription_usage_errors: Vec<crate::commands::usage::UsageProviderError>,
    subscription_provider_ids: Vec<UsageProviderId>,

    pub usage_fetch_attempted: bool,
    usage_initial_fetch_started: bool,
    usage_rx: Option<std::sync::mpsc::Receiver<crate::commands::usage::UsageFetchBatch>>,
}

impl App {
    pub fn new_with_cached_data(config: TuiConfig, cached_data: Option<UsageData>) -> Result<Self> {
        let settings = Settings::load();
        Self::new_with_cached_data_and_settings(config, cached_data, settings)
    }

    fn new_with_cached_data_and_settings(
        config: TuiConfig,
        cached_data: Option<UsageData>,
        settings: Settings,
    ) -> Result<Self> {
        let theme_name: ThemeName = config
            .theme
            .parse()
            .unwrap_or_else(|_| settings.theme_name());
        let theme = Theme::from_name_for_current_terminal(theme_name);

        let enabled_clients: HashSet<ClientId> = if let Some(ref cli_clients) = config.clients {
            // CLI-provided filter list. Each entry is the canonical
            // lowercase client id.
            cli_clients
                .iter()
                .filter_map(|s| ClientId::from_str(&s.to_lowercase()))
                .collect()
        } else {
            // No filter → use the canonical default set. MUST stay in sync with
            // `run_warm_tui_cache()` so a fresh cache warm produces a
            // fresh hit on the next no-filter launch.
            ClientId::iter().collect()
        };

        let auto_refresh_interval = if config.refresh > 0 {
            Duration::from_secs(config.refresh)
        } else if let Some(interval) = settings.get_auto_refresh_interval() {
            interval
        } else {
            Duration::from_secs(30)
        };

        let auto_refresh = config.refresh > 0 || settings.auto_refresh_enabled;
        let usage_tab_enabled = settings.usage_tab_enabled;
        let subscription_provider_ids =
            crate::commands::usage::parse_provider_settings(&settings.usage_providers);

        let data_loader = DataLoader::with_filters(
            config.sessions_path.map(std::path::PathBuf::from),
            config.since,
            config.until,
            config.year,
        );

        let data = cached_data.unwrap_or_default();
        let has_data = !data.models.is_empty();
        let dialog_stack = DialogStack::new(theme.clone());
        let dialog_needs_reload = Rc::new(RefCell::new(false));
        let requested_tab = config.initial_tab.unwrap_or(Tab::Overview);
        let current_tab = if Self::tab_visible(&settings, requested_tab) {
            requested_tab
        } else {
            Tab::Overview
        };
        let (sort_field, sort_direction) = Self::default_sort_for_tab(current_tab);

        let mut app = Self {
            should_quit: false,
            current_tab,
            theme,
            settings,
            data,
            data_loader,
            enabled_clients: Rc::new(RefCell::new(enabled_clients)),
            group_by: Rc::new(RefCell::new(super::cache::TUI_DEFAULT_GROUP_BY)),
            sort_field,
            sort_direction,
            tab_sort_state: HashMap::new(),
            chart_granularity: ChartGranularity::default(),
            scroll_offset: 0,
            selected_index: 0,
            max_visible_items: 20,
            usage_viewport: TextViewport::default(),
            usage_text_total_lines: 0,
            hourly_profile_viewport: TextViewport::default(),
            hourly_profile_text_total_lines: 0,
            selected_daily_detail_date: None,
            daily_list_selected_index: 0,
            daily_list_scroll_offset: 0,
            daily_list_sort_before_detail: None,
            daily_detail_sort_state: None,
            selected_period_detail: None,
            period_list_selected_index: 0,
            period_list_scroll_offset: 0,
            period_list_sort_before_detail: None,
            period_detail_sort_state: None,
            selected_graph_cell: None,
            stats_breakdown_total_lines: 0,
            auto_refresh,
            auto_refresh_interval,
            last_refresh: Instant::now(),
            last_subscription_usage_check: None,
            status_message: if has_data {
                Some("Loaded from cache".to_string())
            } else {
                None
            },
            status_message_time: if has_data { Some(Instant::now()) } else { None },
            terminal_width: 80,
            terminal_height: 24,
            click_areas: Vec::new(),
            spinner_frame: 0,
            background_loading: false,
            blocking_loading: false,
            needs_reload: false,
            reload_force: false,
            last_source_digest: None,
            dialog_stack,
            dialog_needs_reload,
            hourly_view_mode: HourlyViewMode::default(),
            model_shade_map: HashMap::new(),
            subscription_usage: if usage_tab_enabled {
                #[cfg(not(test))]
                {
                    crate::commands::usage::load_cache().unwrap_or_default()
                }
                #[cfg(test)]
                {
                    Vec::new()
                }
            } else {
                Vec::new()
            },
            subscription_usage_errors: Vec::new(),
            subscription_provider_ids,
            usage_fetch_attempted: false,
            usage_initial_fetch_started: false,
            usage_rx: None,
        };
        app.build_model_shade_map();
        app.maybe_fetch_subscription_usage_on_usage_entry();
        Ok(app)
    }

    pub fn set_background_loading(&mut self, loading: bool) {
        self.background_loading = loading;
        if !loading {
            self.blocking_loading = false;
        }
        // Don't set data.loading - let cached data remain visible during background refresh
    }

    pub fn request_blocking_reload(&mut self) {
        self.needs_reload = true;
        self.reload_force = true;
        self.blocking_loading = true;
    }

    /// Marks an auto-refresh probe that found no source changes: resets the
    /// refresh clock without touching the data.
    pub fn mark_refresh_checked(&mut self) {
        let now = Instant::now();
        self.last_refresh = now;
    }

    pub fn is_blocking_loading(&self) -> bool {
        self.blocking_loading
            || (!self.dialog_stack.is_active() && *self.dialog_needs_reload.borrow())
    }

    fn consume_dialog_reload_if_ready(&mut self) {
        let needs_blocking_reload = {
            let mut needs_reload = self.dialog_needs_reload.borrow_mut();
            if !self.dialog_stack.is_active() && *needs_reload {
                *needs_reload = false;
                true
            } else {
                false
            }
        };

        if needs_blocking_reload {
            self.request_blocking_reload();
        }
    }

    pub fn update_data(&mut self, data: UsageData) {
        self.data = data;
        let now = Instant::now();
        self.last_refresh = now;
        self.build_model_shade_map();

        // Exit Daily-detail mode if the refresh dropped the day we were
        // viewing; otherwise `get_sorted_daily_detail_rows()` would return
        // empty while the user is still nominally in detail mode.
        if let Some(date) = self.selected_daily_detail_date {
            if !self.data.daily.iter().any(|day| day.date == date) {
                self.leave_daily_detail_sort_context();
                self.selected_daily_detail_date = None;
                self.selected_index = self.daily_list_selected_index;
                self.scroll_offset = self.daily_list_scroll_offset;
            }
        }
        if let Some(selection) = self.selected_period_detail {
            let period_still_exists = build_period_usage(&self.data.daily, selection.kind)
                .iter()
                .any(|period| {
                    period.start_date == selection.start_date
                        && period.end_date == selection.end_date
                });
            if !period_still_exists {
                self.leave_period_detail_sort_context();
                self.selected_period_detail = None;
                self.selected_index = self.period_list_selected_index;
                self.scroll_offset = self.period_list_scroll_offset;
            }
        }

        self.clamp_selection();
    }

    pub fn build_model_shade_map(&mut self) {
        self.model_shade_map = super::colors::build_model_shade_map(&self.data.models);
    }

    pub fn model_color_for(&self, provider: &str, model: &str) -> Color {
        let provider = provider_color_key(provider);
        let lookup_key = super::colors::model_shade_key(provider, model);
        let color = self
            .model_shade_map
            .get(&lookup_key)
            .copied()
            .unwrap_or_else(|| get_provider_shade(provider, 0));
        self.theme.color(color)
    }

    pub fn model_color(&self, model: &str) -> Color {
        let provider = provider_color_key("");
        let lookup_key = super::colors::model_shade_key(provider, model);
        let color = self
            .model_shade_map
            .get(&lookup_key)
            .copied()
            .unwrap_or_else(|| get_model_color(model));
        self.theme.color(color)
    }

    pub fn has_visible_data(&self) -> bool {
        !self.data.models.is_empty()
            || !self.data.daily.is_empty()
            || !self.data.agents.is_empty()
            || self.data.graph.is_some()
            || self.data.total_tokens > 0
            || self.data.total_cost > 0.0
    }

    pub fn set_error(&mut self, error: Option<String>) {
        self.data.error = error;
    }

    fn refresh_current_tab_if_overdue(&mut self) {
        if !self.auto_refresh {
            return;
        }

        let now = Instant::now();
        if self.current_tab != Tab::Usage
            && self.last_refresh.elapsed() >= self.auto_refresh_interval
            && !self.background_loading
        {
            self.last_refresh = now;
            self.needs_reload = true;
        }
    }

    pub fn on_tick(&mut self) {
        self.spinner_frame = (self.spinner_frame + 1) % 20;

        if let Some(status_time) = self.status_message_time {
            if status_time.elapsed() > Duration::from_secs(3) {
                self.status_message = None;
                self.status_message_time = None;
            }
        }

        self.refresh_current_tab_if_overdue();

        self.consume_dialog_reload_if_ready();

        // Poll background usage fetch
        if let Some(ref rx) = self.usage_rx {
            match rx.try_recv() {
                Ok(batch) => {
                    self.usage_rx = None;
                    self.subscription_usage = batch.outputs;
                    self.subscription_usage_errors = batch.errors;
                    if !self.subscription_usage.is_empty() {
                        crate::commands::usage::save_cache(&self.subscription_usage);
                        if self.subscription_usage_errors.is_empty() {
                            self.status_message = Some("Usage data loaded".into());
                        } else {
                            self.status_message =
                                Some("Usage data loaded with provider errors".into());
                        }
                    } else {
                        crate::commands::usage::clear_cache();
                        if self.subscription_usage_errors.is_empty() {
                            self.status_message = Some("No usage data available".into());
                        } else {
                            self.status_message = Some("Usage fetch failed".into());
                        }
                    }
                    let now = std::time::Instant::now();
                    self.last_subscription_usage_check = Some(now);
                    self.status_message_time = Some(now);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.usage_rx = None;
                    self.subscription_usage_errors =
                        vec![crate::commands::usage::UsageProviderError {
                            provider: "unknown".to_string(),
                            message: "usage fetch worker disconnected".to_string(),
                        }];
                    let now = std::time::Instant::now();
                    self.last_subscription_usage_check = Some(now);
                    self.status_message = Some("Usage fetch failed".into());
                    self.status_message_time = Some(now);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return true;
        }

        if self.dialog_stack.is_active() {
            self.dialog_stack.handle_key(key);
            self.consume_dialog_reload_if_ready();
            return false;
        }

        if let Some(command) = move_command_from_key(key.code) {
            if self.apply_text_viewport_move(command) {
                return false;
            }
        }

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return true;
            }
            KeyCode::Tab => {
                let next = self.next_visible_tab();
                self.switch_tab(next);
                self.reset_selection();
            }
            KeyCode::BackTab => {
                let prev = self.prev_visible_tab();
                self.switch_tab(prev);
                self.reset_selection();
            }
            KeyCode::Left => {
                let prev = self.prev_visible_tab();
                self.switch_tab(prev);
                self.reset_selection();
            }
            KeyCode::Right => {
                let next = self.next_visible_tab();
                self.switch_tab(next);
                self.reset_selection();
            }
            KeyCode::Up => {
                self.move_selection_up();
            }
            KeyCode::Down => {
                self.move_selection_down();
            }
            KeyCode::PageUp => {
                self.move_page_up();
            }
            KeyCode::PageDown => {
                self.move_page_down();
            }
            KeyCode::Home => {
                self.move_to_top();
            }
            KeyCode::End => {
                self.move_to_bottom();
            }
            KeyCode::Char('c') => {
                self.set_sort(SortField::Cost);
            }
            KeyCode::Char('t') => {
                self.set_sort(SortField::Tokens);
            }
            KeyCode::Char('d') => {
                self.set_sort(SortField::Date);
            }
            KeyCode::Char('j') => {
                self.jump_to_today();
            }
            KeyCode::Char('p') => {
                self.cycle_theme();
            }
            KeyCode::Char('r') => {
                let now = Instant::now();
                self.last_refresh = now;
                if self.background_loading {
                    self.set_status("Refresh already in progress");
                } else {
                    self.needs_reload = true;
                    self.reload_force = true;
                }
            }
            KeyCode::Char('R') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.toggle_auto_refresh();
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.increase_refresh_interval();
            }
            KeyCode::Char('-') => {
                self.decrease_refresh_interval();
            }
            KeyCode::Char('y') => {
                self.copy_selected_to_clipboard();
            }
            KeyCode::Char('e') => {
                self.export_to_json();
            }
            KeyCode::Char('s') => {
                self.open_client_picker();
            }
            KeyCode::Char('h') if self.current_tab == Tab::Overview => {
                self.chart_granularity = match self.chart_granularity {
                    ChartGranularity::Daily => ChartGranularity::Hourly,
                    ChartGranularity::Hourly => ChartGranularity::Daily,
                };
            }
            KeyCode::Char('v') if self.current_tab == Tab::Hourly => {
                self.hourly_view_mode = match self.hourly_view_mode {
                    HourlyViewMode::Table => HourlyViewMode::Profile,
                    HourlyViewMode::Profile => HourlyViewMode::Table,
                };
                self.reset_selection();
            }
            KeyCode::Char('g') => {
                self.open_group_by_picker();
            }
            KeyCode::Char('u') if self.current_tab == Tab::Usage => {
                self.fetch_subscription_usage();
            }
            KeyCode::Enter if self.current_tab == Tab::Daily => {
                self.open_selected_daily_detail();
            }
            KeyCode::Enter if self.current_tab == Tab::Monthly => {
                self.open_selected_period_detail(PeriodKind::Monthly);
            }
            KeyCode::Enter if self.current_tab == Tab::Weekly => {
                self.open_selected_period_detail(PeriodKind::Weekly);
            }
            KeyCode::Enter if self.current_tab == Tab::Stats => {
                self.handle_graph_selection();
            }
            KeyCode::Esc | KeyCode::Backspace
                if self.current_tab == Tab::Daily && self.is_daily_detail_active() =>
            {
                self.close_daily_detail();
            }
            KeyCode::Esc | KeyCode::Backspace
                if self.current_period_kind().is_some() && self.is_period_detail_active() =>
            {
                self.close_period_detail();
            }
            KeyCode::Esc if self.selected_graph_cell.is_some() => {
                self.selected_graph_cell = None;
                self.stats_breakdown_total_lines = 0;
                self.selected_index = 0;
                self.scroll_offset = 0;
            }
            _ => {}
        }
        false
    }

    pub fn fetch_subscription_usage(&mut self) {
        if self.usage_rx.is_some() {
            self.set_status("Subscription usage fetch already in progress");
            return;
        }
        if self.subscription_provider_ids.is_empty() {
            self.status_message = Some("No subscription usage providers enabled".into());
            self.status_message_time = Some(std::time::Instant::now());
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.usage_fetch_attempted = true;
        self.status_message = Some("Fetching subscription usage...".into());
        self.status_message_time = Some(std::time::Instant::now());
        self.usage_rx = Some(rx);
        let enabled = self.subscription_provider_ids.clone();
        std::thread::spawn(move || {
            let batch = crate::commands::usage::fetch_enabled(&enabled);
            let _ = tx.send(batch);
        });
    }

    fn should_start_initial_subscription_usage_fetch(&self) -> bool {
        self.current_tab == Tab::Usage
            && self.settings.usage_tab_enabled
            && !self.usage_initial_fetch_started
            && !self.subscription_provider_ids.is_empty()
    }

    fn maybe_fetch_subscription_usage_on_usage_entry(&mut self) {
        if !self.should_start_initial_subscription_usage_fetch() {
            return;
        }
        self.usage_initial_fetch_started = true;
        self.fetch_subscription_usage();
    }

    #[cfg(test)]
    fn start_subscription_usage_fetch_for_test(
        &mut self,
        rx: std::sync::mpsc::Receiver<crate::commands::usage::UsageFetchBatch>,
    ) {
        self.usage_fetch_attempted = true;
        self.status_message = Some("Fetching subscription usage...".into());
        self.status_message_time = Some(std::time::Instant::now());
        self.usage_rx = Some(rx);
    }

    pub fn is_fetching_usage(&self) -> bool {
        self.usage_rx.is_some()
    }

    pub fn handle_mouse_event(&mut self, event: MouseEvent) {
        if self.dialog_stack.is_active() {
            self.dialog_stack.handle_mouse(event);
            self.consume_dialog_reload_if_ready();
            return;
        }

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let x = event.column;
                let y = event.row;

                for area in &self.click_areas {
                    if x >= area.rect.x
                        && x < area.rect.x + area.rect.width
                        && y >= area.rect.y
                        && y < area.rect.y + area.rect.height
                    {
                        match &area.action {
                            ClickAction::Tab(tab) => {
                                self.switch_tab(*tab);
                                self.reset_selection();
                            }
                            ClickAction::Sort(field) => {
                                self.set_sort(*field);
                            }
                            ClickAction::GraphCell { week, day } => {
                                self.selected_graph_cell = Some((*week, *day));
                                self.stats_breakdown_total_lines = 0;
                                self.selected_index = 0;
                                self.scroll_offset = 0;
                            }
                        }
                        break;
                    }
                }
            }
            MouseEventKind::ScrollUp if !self.scroll_active_text_viewport(MoveCommand::Up) => {
                self.move_selection_up();
            }
            MouseEventKind::ScrollDown if !self.scroll_active_text_viewport(MoveCommand::Down) => {
                self.move_selection_down();
            }
            _ => {}
        }
    }

    /// Cache the latest terminal dimensions. `max_visible_items` is
    /// intentionally not updated here: each tab's renderer owns its own
    /// visible-item capacity and pushes the rendered count via
    /// [`Self::set_max_visible_items`] (which clamps selection and scroll
    /// state). Between resize and the next render, scroll math runs
    /// against the previous tab's capacity for one frame and self-corrects.
    pub fn handle_resize(&mut self, width: u16, height: u16) {
        self.terminal_width = width;
        self.terminal_height = height;
    }

    pub(crate) fn set_max_visible_items(&mut self, max_visible_items: usize) {
        self.max_visible_items = max_visible_items.max(1);
        self.clamp_selection();
    }

    pub(crate) fn set_usage_text_viewport(&mut self, visible: usize, total_lines: usize) {
        self.usage_text_total_lines = total_lines;
        self.usage_viewport.set_visible(visible, total_lines);
    }

    pub(crate) fn usage_text_visible_range(&self) -> std::ops::Range<usize> {
        self.usage_viewport
            .visible_range(self.usage_text_total_lines)
    }

    pub(crate) fn set_hourly_profile_text_viewport(&mut self, visible: usize, total_lines: usize) {
        self.hourly_profile_text_total_lines = total_lines;
        self.hourly_profile_viewport
            .set_visible(visible, total_lines);
    }

    pub(crate) fn hourly_profile_text_visible_range(&self) -> std::ops::Range<usize> {
        self.hourly_profile_viewport
            .visible_range(self.hourly_profile_text_total_lines)
    }

    fn active_text_viewport_mut(&mut self) -> Option<&mut TextViewport> {
        match self.current_tab {
            Tab::Usage => Some(&mut self.usage_viewport),
            Tab::Hourly if self.hourly_view_mode == HourlyViewMode::Profile => {
                Some(&mut self.hourly_profile_viewport)
            }
            _ => None,
        }
    }

    fn current_text_total_lines(&self) -> Option<usize> {
        match self.current_tab {
            Tab::Usage => Some(self.usage_text_total_lines),
            Tab::Hourly if self.hourly_view_mode == HourlyViewMode::Profile => {
                Some(self.hourly_profile_text_total_lines)
            }
            _ => None,
        }
    }

    fn apply_text_viewport_move(&mut self, command: MoveCommand) -> bool {
        let Some(total_lines) = self.current_text_total_lines() else {
            return false;
        };
        let Some(viewport) = self.active_text_viewport_mut() else {
            return false;
        };

        let _ = viewport.apply_move(command, total_lines);
        true
    }

    fn scroll_active_text_viewport(&mut self, command: MoveCommand) -> bool {
        self.apply_text_viewport_move(command)
    }

    /// Clamp selection and scroll offset to valid bounds after data/resize changes.
    /// Stats breakdown is skipped here because `render_breakdown_panel` clamps
    /// with the actual panel height (not the full-terminal `max_visible_items`).
    fn clamp_selection(&mut self) {
        if self.current_tab == Tab::Stats && self.selected_graph_cell.is_some() {
            return;
        }
        let len = self.get_current_list_len();
        if len == 0 {
            self.selected_index = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected_index = self.selected_index.min(len.saturating_sub(1));
        let max_scroll = len.saturating_sub(self.max_visible_items);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    pub fn clear_click_areas(&mut self) {
        self.click_areas.clear();
    }

    pub fn add_click_area(&mut self, rect: Rect, action: ClickAction) {
        self.click_areas.push(ClickArea { rect, action });
    }

    fn reset_selection(&mut self) {
        self.scroll_offset = 0;
        self.selected_index = 0;
        self.usage_viewport.scroll = 0;
        self.hourly_profile_viewport.scroll = 0;
        self.selected_daily_detail_date = None;
        self.daily_list_selected_index = 0;
        self.daily_list_scroll_offset = 0;
        self.daily_list_sort_before_detail = None;
        self.selected_period_detail = None;
        self.period_list_selected_index = 0;
        self.period_list_scroll_offset = 0;
        self.period_list_sort_before_detail = None;
        self.selected_graph_cell = None;
        self.stats_breakdown_total_lines = 0;
    }

    fn switch_tab(&mut self, target: Tab) {
        if !self.is_tab_visible(target) {
            return;
        }

        let was_daily_detail = self.current_tab == Tab::Daily && self.is_daily_detail_active();
        let was_period_detail = self.is_period_detail_active();
        self.persist_current_sort();

        self.current_tab = target;
        if target != Tab::Daily || was_daily_detail {
            self.selected_daily_detail_date = None;
            self.daily_list_sort_before_detail = None;
        }
        if was_period_detail {
            self.selected_period_detail = None;
            self.period_list_sort_before_detail = None;
        }

        let (field, dir) = self
            .tab_sort_state
            .get(&target)
            .copied()
            .unwrap_or_else(|| Self::default_sort_for_tab(target));
        self.sort_field = field;
        self.sort_direction = dir;
        self.refresh_current_tab_if_overdue();
        self.maybe_fetch_subscription_usage_on_usage_entry();
    }

    fn default_sort_for_tab(tab: Tab) -> (SortField, SortDirection) {
        match tab {
            Tab::Models => (SortField::Tokens, SortDirection::Descending),
            Tab::Monthly | Tab::Weekly | Tab::Daily | Tab::Hourly => {
                (SortField::Date, SortDirection::Descending)
            }
            Tab::Overview | Tab::Usage | Tab::Stats | Tab::Agents => {
                (SortField::Cost, SortDirection::Descending)
            }
        }
    }

    fn default_sort_for_daily_detail() -> (SortField, SortDirection) {
        (SortField::Tokens, SortDirection::Descending)
    }

    fn default_sort_for_period_detail() -> (SortField, SortDirection) {
        (SortField::Tokens, SortDirection::Descending)
    }

    pub(crate) fn tab_visible(settings: &Settings, tab: Tab) -> bool {
        match tab {
            Tab::Usage => settings.usage_tab_enabled,
            _ => true,
        }
    }

    fn period_tab(kind: PeriodKind) -> Tab {
        match kind {
            PeriodKind::Monthly => Tab::Monthly,
            PeriodKind::Weekly => Tab::Weekly,
        }
    }

    fn current_period_kind(&self) -> Option<PeriodKind> {
        match self.current_tab {
            Tab::Monthly => Some(PeriodKind::Monthly),
            Tab::Weekly => Some(PeriodKind::Weekly),
            _ => None,
        }
    }

    pub(crate) fn is_tab_visible(&self, tab: Tab) -> bool {
        Self::tab_visible(&self.settings, tab)
    }

    fn next_visible_tab(&self) -> Tab {
        let mut candidate = self.current_tab.next();
        while !self.is_tab_visible(candidate) && candidate != self.current_tab {
            candidate = candidate.next();
        }
        candidate
    }

    fn prev_visible_tab(&self) -> Tab {
        let mut candidate = self.current_tab.prev();
        while !self.is_tab_visible(candidate) && candidate != self.current_tab {
            candidate = candidate.prev();
        }
        candidate
    }

    fn persist_current_sort(&mut self) {
        let current_sort = (self.sort_field, self.sort_direction);
        if self.current_tab == Tab::Daily && self.is_daily_detail_active() {
            self.daily_detail_sort_state = Some(current_sort);
            let daily_sort = self
                .daily_list_sort_before_detail
                .unwrap_or_else(|| Self::default_sort_for_tab(Tab::Daily));
            self.tab_sort_state.insert(Tab::Daily, daily_sort);
            return;
        }
        if let Some(selection) = self.selected_period_detail {
            self.period_detail_sort_state = Some(current_sort);
            let tab = Self::period_tab(selection.kind);
            let period_sort = self
                .period_list_sort_before_detail
                .unwrap_or_else(|| Self::default_sort_for_tab(tab));
            self.tab_sort_state.insert(tab, period_sort);
            return;
        }

        self.tab_sort_state.insert(self.current_tab, current_sort);
    }

    fn enter_daily_detail_sort_context(&mut self) {
        self.daily_list_sort_before_detail = Some((self.sort_field, self.sort_direction));
        let (field, direction) = self
            .daily_detail_sort_state
            .unwrap_or_else(Self::default_sort_for_daily_detail);
        self.sort_field = field;
        self.sort_direction = direction;
    }

    fn leave_daily_detail_sort_context(&mut self) {
        self.daily_detail_sort_state = Some((self.sort_field, self.sort_direction));
        let daily_sort = self
            .daily_list_sort_before_detail
            .take()
            .or_else(|| self.tab_sort_state.get(&Tab::Daily).copied())
            .unwrap_or_else(|| Self::default_sort_for_tab(Tab::Daily));
        self.sort_field = daily_sort.0;
        self.sort_direction = daily_sort.1;
        self.tab_sort_state.insert(Tab::Daily, daily_sort);
    }

    fn enter_period_detail_sort_context(&mut self) {
        self.period_list_sort_before_detail = Some((self.sort_field, self.sort_direction));
        let (field, direction) = self
            .period_detail_sort_state
            .unwrap_or_else(Self::default_sort_for_period_detail);
        self.sort_field = field;
        self.sort_direction = direction;
    }

    fn leave_period_detail_sort_context(&mut self) {
        self.period_detail_sort_state = Some((self.sort_field, self.sort_direction));
        let tab = self
            .selected_period_detail
            .map(|selection| Self::period_tab(selection.kind))
            .unwrap_or(self.current_tab);
        let period_sort = self
            .period_list_sort_before_detail
            .take()
            .or_else(|| self.tab_sort_state.get(&tab).copied())
            .unwrap_or_else(|| Self::default_sort_for_tab(tab));
        self.sort_field = period_sort.0;
        self.sort_direction = period_sort.1;
        self.tab_sort_state.insert(tab, period_sort);
    }

    fn move_selection_up(&mut self) {
        self.apply_list_move(MoveCommand::Up);
    }

    fn move_selection_down(&mut self) {
        self.apply_list_move(MoveCommand::Down);
    }

    fn move_page_up(&mut self) {
        self.apply_list_move(MoveCommand::PageUp);
    }

    fn move_page_down(&mut self) {
        self.apply_list_move(MoveCommand::PageDown);
    }

    fn move_to_top(&mut self) {
        self.apply_list_move(MoveCommand::Home);
    }

    fn move_to_bottom(&mut self) {
        self.apply_list_move(MoveCommand::End);
    }

    fn apply_list_move(&mut self, command: MoveCommand) -> InteractionOutcome {
        let len = self.get_current_list_len();
        let wrap = if self.current_tab == Tab::Stats && self.selected_graph_cell.is_some() {
            WrapMode::Clamp
        } else {
            WrapMode::Wrap
        };

        let mut interaction = ListInteraction {
            selected: self.selected_index,
            scroll: self.scroll_offset,
            visible: self.max_visible_items,
        };
        let outcome = interaction.apply_move(command, len, wrap);
        self.selected_index = interaction.selected;
        self.scroll_offset = interaction.scroll;
        self.max_visible_items = interaction.visible;
        outcome
    }

    fn get_current_list_len(&self) -> usize {
        match self.current_tab {
            Tab::Overview | Tab::Models => self.data.models.len(),
            Tab::Agents => self.data.agents.len(),
            Tab::Daily if self.is_daily_detail_active() => {
                self.get_sorted_daily_detail_rows().len()
            }
            Tab::Monthly if self.is_period_detail_active_for_kind(PeriodKind::Monthly) => {
                self.get_sorted_period_detail_rows().len()
            }
            Tab::Weekly if self.is_period_detail_active_for_kind(PeriodKind::Weekly) => {
                self.get_sorted_period_detail_rows().len()
            }
            Tab::Monthly => build_period_usage(&self.data.daily, PeriodKind::Monthly).len(),
            Tab::Weekly => build_period_usage(&self.data.daily, PeriodKind::Weekly).len(),
            Tab::Daily => self.data.daily.len(),
            Tab::Hourly => self.data.hourly.len(),
            Tab::Stats => {
                if self.selected_graph_cell.is_some() {
                    self.stats_breakdown_total_lines
                } else {
                    0
                }
            }
            Tab::Usage => self
                .subscription_usage
                .iter()
                .map(|u| u.metrics.len())
                .sum(),
        }
    }

    fn set_sort(&mut self, field: SortField) {
        if self.sort_field == field {
            self.sort_direction = match self.sort_direction {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
        } else {
            self.sort_field = field;
            self.sort_direction = SortDirection::Descending;
        }
        self.persist_current_sort();
        if (self.current_tab == Tab::Daily && self.is_daily_detail_active())
            || self.is_period_detail_active()
        {
            self.selected_index = 0;
            self.scroll_offset = 0;
        } else {
            self.reset_selection();
        }
        self.set_status(&format!(
            "Sorted by {:?} {:?}",
            self.sort_field, self.sort_direction
        ));
    }

    fn jump_to_today(&mut self) {
        if self.current_tab != Tab::Daily {
            return;
        }
        if self.is_daily_detail_active() {
            self.leave_daily_detail_sort_context();
        }
        self.selected_daily_detail_date = None;

        let today = chrono::Local::now().date_naive();
        let (today_index, total_len) = {
            let sorted_daily = self.get_sorted_daily();
            (
                sorted_daily.iter().position(|d| d.date == today),
                sorted_daily.len(),
            )
        };

        if let Some(index) = today_index {
            self.selected_index = index;

            if self.max_visible_items > 0 {
                let max_scroll = total_len.saturating_sub(self.max_visible_items);
                self.scroll_offset = index
                    .saturating_sub(self.max_visible_items / 2)
                    .min(max_scroll);
            } else {
                self.scroll_offset = 0;
            }

            self.selected_graph_cell = None;
            self.set_status("Jumped to today's usage");
        } else {
            self.set_status("No usage recorded for today");
        }
    }

    fn cycle_theme(&mut self) {
        let new_theme = self.theme.name.next();
        self.theme = Theme::from_name_for_current_terminal(new_theme);
        self.dialog_stack.set_theme(self.theme.clone());
        self.settings.set_theme(new_theme);
        if let Err(e) = self.settings.save() {
            self.set_status(&format!(
                "Theme: {} (save failed: {})",
                new_theme.as_str(),
                e
            ));
        } else {
            self.set_status(&format!("Theme: {}", new_theme.as_str()));
        }
    }

    fn open_client_picker(&mut self) {
        let dialog = ClientPickerDialog::new(
            self.enabled_clients.clone(),
            self.dialog_needs_reload.clone(),
        );
        self.dialog_stack.show(Box::new(dialog));
    }

    pub fn scan_clients(&self) -> Vec<ClientId> {
        let mut out: Vec<ClientId> = self.enabled_clients.borrow().iter().copied().collect();
        // Stable order for downstream cache key + log output. Sort by the
        // declaration index in ClientId::ALL so the projection mirrors
        // the canonical ordering used elsewhere.
        out.sort_by_key(|c| *c as usize);
        out
    }

    fn open_group_by_picker(&mut self) {
        use super::ui::dialog::GroupByPickerDialog;
        let dialog =
            GroupByPickerDialog::new(self.group_by.clone(), self.dialog_needs_reload.clone());
        self.dialog_stack.show(Box::new(dialog));
    }

    fn open_selected_daily_detail(&mut self) {
        if self.is_daily_detail_active() {
            return;
        }

        let selected_date = {
            let daily = self.get_sorted_daily();
            daily.get(self.selected_index).map(|day| day.date)
        };

        if let Some(date) = selected_date {
            self.daily_list_selected_index = self.selected_index;
            self.daily_list_scroll_offset = self.scroll_offset;
            self.selected_daily_detail_date = Some(date);
            self.enter_daily_detail_sort_context();
            self.selected_index = 0;
            self.scroll_offset = 0;
            self.set_status(&format!("Viewing daily details for {}", date));
            self.clamp_selection();
        }
    }

    fn close_daily_detail(&mut self) {
        let Some(detail_date) = self.selected_daily_detail_date else {
            return;
        };

        self.leave_daily_detail_sort_context();
        self.selected_daily_detail_date = None;

        // Re-anchor by date so a sort change inside detail mode still
        // restores the same day rather than the stale list index.
        let restored_index = self
            .get_sorted_daily()
            .iter()
            .position(|day| day.date == detail_date)
            .unwrap_or(self.daily_list_selected_index);

        self.selected_index = restored_index;

        let max_visible = self.max_visible_items.max(1);
        let viewport_still_holds = restored_index >= self.daily_list_scroll_offset
            && restored_index < self.daily_list_scroll_offset + max_visible;
        self.scroll_offset = if viewport_still_holds {
            self.daily_list_scroll_offset
        } else {
            restored_index.saturating_sub(max_visible / 2)
        };

        self.set_status("Returned to daily usage");
        self.clamp_selection();
    }

    fn open_selected_period_detail(&mut self, kind: PeriodKind) {
        if self.is_period_detail_active() {
            return;
        }

        let selected_period = {
            let periods = self.get_sorted_periods(kind);
            periods.get(self.selected_index).map(|period| {
                (
                    PeriodDetailSelection {
                        kind,
                        start_date: period.start_date,
                        end_date: period.end_date,
                    },
                    format!("{} {}", period.section_label, period.label),
                )
            })
        };

        if let Some((selection, label)) = selected_period {
            self.period_list_selected_index = self.selected_index;
            self.period_list_scroll_offset = self.scroll_offset;
            self.selected_period_detail = Some(selection);
            self.enter_period_detail_sort_context();
            self.selected_index = 0;
            self.scroll_offset = 0;
            self.set_status(&format!("Viewing period details for {}", label));
            self.clamp_selection();
        }
    }

    fn close_period_detail(&mut self) {
        let Some(selection) = self.selected_period_detail else {
            return;
        };

        self.leave_period_detail_sort_context();
        self.selected_period_detail = None;

        let restored_index = self
            .get_sorted_periods(selection.kind)
            .iter()
            .position(|period| {
                period.start_date == selection.start_date && period.end_date == selection.end_date
            })
            .unwrap_or(self.period_list_selected_index);

        self.selected_index = restored_index;

        let max_visible = self.max_visible_items.max(1);
        let viewport_still_holds = restored_index >= self.period_list_scroll_offset
            && restored_index < self.period_list_scroll_offset + max_visible;
        self.scroll_offset = if viewport_still_holds {
            self.period_list_scroll_offset
        } else {
            restored_index.saturating_sub(max_visible / 2)
        };

        self.set_status(match selection.kind {
            PeriodKind::Monthly => "Returned to monthly usage",
            PeriodKind::Weekly => "Returned to weekly usage",
        });
        self.clamp_selection();
    }

    fn toggle_auto_refresh(&mut self) {
        self.auto_refresh = !self.auto_refresh;
        if self.auto_refresh {
            let now = Instant::now();
            self.last_refresh = now;
        }
        self.settings.auto_refresh_enabled = self.auto_refresh;
        let save_result = self.settings.save();
        let msg = if self.auto_refresh {
            format!(
                "Auto-refresh ON ({}s)",
                self.auto_refresh_interval.as_secs()
            )
        } else {
            "Auto-refresh OFF".to_string()
        };
        if let Err(e) = save_result {
            self.set_status(&format!("{} (save failed: {})", msg, e));
        } else {
            self.set_status(&msg);
        }
    }

    fn increase_refresh_interval(&mut self) {
        let ms = self.auto_refresh_interval.as_millis() as u64;
        let new_ms = ms.saturating_add(10_000).min(300_000);
        self.auto_refresh_interval = Duration::from_millis(new_ms);
        self.settings.auto_refresh_ms = new_ms;
        let save_result = self.settings.save();
        let msg = format!("Refresh interval: {}s", new_ms / 1000);
        if let Err(e) = save_result {
            self.set_status(&format!("{} (save failed: {})", msg, e));
        } else {
            self.set_status(&msg);
        }
    }

    fn decrease_refresh_interval(&mut self) {
        let ms = self.auto_refresh_interval.as_millis() as u64;
        let new_ms = ms.saturating_sub(10_000).max(30_000);
        self.auto_refresh_interval = Duration::from_millis(new_ms);
        self.settings.auto_refresh_ms = new_ms;
        let save_result = self.settings.save();
        let msg = format!("Refresh interval: {}s", new_ms / 1000);
        if let Err(e) = save_result {
            self.set_status(&format!("{} (save failed: {})", msg, e));
        } else {
            self.set_status(&msg);
        }
    }

    fn copy_selected_to_clipboard(&mut self) {
        let text = match self.current_tab {
            Tab::Overview | Tab::Models => self
                .get_sorted_models()
                .get(self.selected_index)
                .map(|m| format!("{}: {} tokens, ${:.4}", m.model, m.tokens.total(), m.cost)),
            Tab::Agents => self.get_sorted_agents().get(self.selected_index).map(|a| {
                format!(
                    "{}: {} tokens, ${:.4}, {} instances",
                    a.agent,
                    a.tokens.total(),
                    a.cost,
                    a.instance_count
                )
            }),
            Tab::Daily if self.is_daily_detail_active() => self
                .get_sorted_daily_detail_rows()
                .get(self.selected_index)
                .map(|row| {
                    format!(
                        "{} / {}: {} tokens, ${:.4}",
                        row.source,
                        row.model,
                        row.tokens.total(),
                        row.cost
                    )
                }),
            Tab::Monthly | Tab::Weekly if self.is_period_detail_active() => self
                .get_sorted_period_detail_rows()
                .get(self.selected_index)
                .map(|row| {
                    format!(
                        "{} / {}: {} tokens, ${:.4}",
                        row.source,
                        row.model,
                        row.tokens.total(),
                        row.cost
                    )
                }),
            Tab::Daily => self
                .get_sorted_daily()
                .get(self.selected_index)
                .map(|d| format!("{}: {} tokens, ${:.4}", d.date, d.tokens.total(), d.cost)),
            Tab::Monthly => self
                .get_sorted_periods(PeriodKind::Monthly)
                .get(self.selected_index)
                .map(|p| {
                    format!(
                        "{} {}: {} tokens, ${:.4}",
                        p.section_label,
                        p.label,
                        p.tokens.total(),
                        p.cost
                    )
                }),
            Tab::Weekly => self
                .get_sorted_periods(PeriodKind::Weekly)
                .get(self.selected_index)
                .map(|p| {
                    format!(
                        "{} {}: {} tokens, ${:.4}",
                        p.section_label,
                        p.label,
                        p.tokens.total(),
                        p.cost
                    )
                }),
            Tab::Hourly => self.get_sorted_hourly().get(self.selected_index).map(|h| {
                format!(
                    "{}: {} tokens, ${:.4}",
                    h.datetime.format("%Y-%m-%d %H:%M"),
                    h.tokens.total(),
                    h.cost
                )
            }),
            Tab::Stats | Tab::Usage => None,
        };

        if let Some(text) = text {
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&text)) {
                Ok(_) => self.set_status("Copied to clipboard"),
                Err(_) => self.set_status("Failed to copy"),
            }
        }
    }

    fn export_to_json(&mut self) {
        let filename = format!(
            "tokscale-export-{}.json",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );

        match super::export::build_export_json(&self.data) {
            Ok(json) => match std::fs::write(&filename, json) {
                Ok(_) => self.set_status(&format!("Exported to {}", filename)),
                Err(e) => self.set_status(&format!("Export failed: {}", e)),
            },
            Err(e) => self.set_status(&format!("Export failed: {}", e)),
        }
    }

    fn handle_graph_selection(&mut self) {
        if self.current_tab == Tab::Stats && self.selected_graph_cell.is_some() {
            self.set_status("Press ESC to deselect");
        }
    }

    pub fn set_status(&mut self, message: &str) {
        self.status_message = Some(message.to_string());
        self.status_message_time = Some(Instant::now());
    }

    pub fn get_sorted_models(&self) -> Vec<&ModelUsage> {
        let mut models: Vec<&ModelUsage> = self.data.models.iter().collect();

        let tie_breaker = |a: &&ModelUsage, b: &&ModelUsage| {
            a.model
                .cmp(&b.model)
                .then_with(|| a.workspace_label.cmp(&b.workspace_label))
                .then_with(|| a.workspace_key.cmp(&b.workspace_key))
                .then_with(|| a.provider.cmp(&b.provider))
                .then_with(|| a.client.cmp(&b.client))
        };

        match (self.sort_field, self.sort_direction) {
            (SortField::Cost, SortDirection::Descending) => {
                models.sort_by(|a, b| b.cost.total_cmp(&a.cost).then_with(|| tie_breaker(a, b)))
            }
            (SortField::Cost, SortDirection::Ascending) => {
                models.sort_by(|a, b| a.cost.total_cmp(&b.cost).then_with(|| tie_breaker(a, b)))
            }
            (SortField::Tokens, SortDirection::Descending) => models.sort_by(|a, b| {
                b.tokens
                    .total()
                    .cmp(&a.tokens.total())
                    .then_with(|| tie_breaker(a, b))
            }),
            (SortField::Tokens, SortDirection::Ascending) => models.sort_by(|a, b| {
                a.tokens
                    .total()
                    .cmp(&b.tokens.total())
                    .then_with(|| tie_breaker(a, b))
            }),
            (SortField::Date, _) => {
                models.sort_by(|a, b| tie_breaker(a, b));
            }
        }

        models
    }

    pub fn get_sorted_agents(&self) -> Vec<&AgentUsage> {
        let mut agents: Vec<&AgentUsage> = self.data.agents.iter().collect();

        let tie_breaker = |a: &&AgentUsage, b: &&AgentUsage| {
            a.agent
                .cmp(&b.agent)
                .then_with(|| a.clients.cmp(&b.clients))
        };

        match (self.sort_field, self.sort_direction) {
            (SortField::Cost, SortDirection::Descending) => {
                agents.sort_by(|a, b| b.cost.total_cmp(&a.cost).then_with(|| tie_breaker(a, b)))
            }
            (SortField::Cost, SortDirection::Ascending) => {
                agents.sort_by(|a, b| a.cost.total_cmp(&b.cost).then_with(|| tie_breaker(a, b)))
            }
            (SortField::Tokens, SortDirection::Descending) => agents.sort_by(|a, b| {
                b.tokens
                    .total()
                    .cmp(&a.tokens.total())
                    .then_with(|| tie_breaker(a, b))
            }),
            (SortField::Tokens, SortDirection::Ascending) => agents.sort_by(|a, b| {
                a.tokens
                    .total()
                    .cmp(&b.tokens.total())
                    .then_with(|| tie_breaker(a, b))
            }),
            (SortField::Date, _) => {
                agents.sort_by(|a, b| tie_breaker(a, b));
            }
        }

        agents
    }

    pub fn get_sorted_daily(&self) -> Vec<&DailyUsage> {
        let mut daily: Vec<&DailyUsage> = self.data.daily.iter().collect();

        match (self.sort_field, self.sort_direction) {
            (SortField::Cost, SortDirection::Descending) => {
                daily.sort_by(|a, b| b.cost.total_cmp(&a.cost).then_with(|| a.date.cmp(&b.date)))
            }
            (SortField::Cost, SortDirection::Ascending) => {
                daily.sort_by(|a, b| a.cost.total_cmp(&b.cost).then_with(|| a.date.cmp(&b.date)))
            }
            (SortField::Tokens, SortDirection::Descending) => daily.sort_by(|a, b| {
                b.tokens
                    .total()
                    .cmp(&a.tokens.total())
                    .then_with(|| a.date.cmp(&b.date))
            }),
            (SortField::Tokens, SortDirection::Ascending) => daily.sort_by(|a, b| {
                a.tokens
                    .total()
                    .cmp(&b.tokens.total())
                    .then_with(|| a.date.cmp(&b.date))
            }),
            (SortField::Date, SortDirection::Descending) => {
                daily.sort_by_key(|b| std::cmp::Reverse(b.date))
            }
            (SortField::Date, SortDirection::Ascending) => daily.sort_by_key(|a| a.date),
        }

        daily
    }

    pub fn is_daily_detail_active(&self) -> bool {
        self.selected_daily_detail_date.is_some()
    }

    pub fn daily_detail_date(&self) -> Option<NaiveDate> {
        self.selected_daily_detail_date
    }

    pub fn is_period_detail_active(&self) -> bool {
        self.selected_period_detail.is_some()
    }

    pub fn is_period_detail_active_for_kind(&self, kind: PeriodKind) -> bool {
        self.selected_period_detail
            .is_some_and(|selection| selection.kind == kind)
    }

    pub fn period_detail_label(&self) -> Option<String> {
        let selection = self.selected_period_detail?;
        build_period_usage(&self.data.daily, selection.kind)
            .into_iter()
            .find(|period| {
                period.start_date == selection.start_date && period.end_date == selection.end_date
            })
            .map(|period| format!("{} {}", period.section_label, period.label))
    }

    pub fn get_sorted_daily_detail_rows(&self) -> Vec<DailyDetailRow> {
        let Some(date) = self.selected_daily_detail_date else {
            return Vec::new();
        };
        let Some(day) = self.data.daily.iter().find(|day| day.date == date) else {
            return Vec::new();
        };

        let mut rows = build_detail_rows(&day.source_breakdown);
        sort_detail_rows(&mut rows, self.sort_field, self.sort_direction);
        rows
    }

    pub fn get_sorted_period_detail_rows(&self) -> Vec<PeriodDetailRow> {
        let Some(selection) = self.selected_period_detail else {
            return Vec::new();
        };
        let Some(period) = build_period_usage(&self.data.daily, selection.kind)
            .into_iter()
            .find(|period| {
                period.start_date == selection.start_date && period.end_date == selection.end_date
            })
        else {
            return Vec::new();
        };

        let mut rows = build_detail_rows(&period.source_breakdown);
        sort_detail_rows(&mut rows, self.sort_field, self.sort_direction);
        rows
    }

    pub fn get_sorted_hourly(&self) -> Vec<&HourlyUsage> {
        let mut hourly: Vec<&HourlyUsage> = self.data.hourly.iter().collect();

        match (self.sort_field, self.sort_direction) {
            (SortField::Cost, SortDirection::Descending) => hourly.sort_by(|a, b| {
                b.cost
                    .total_cmp(&a.cost)
                    .then_with(|| a.datetime.cmp(&b.datetime))
            }),
            (SortField::Cost, SortDirection::Ascending) => hourly.sort_by(|a, b| {
                a.cost
                    .total_cmp(&b.cost)
                    .then_with(|| a.datetime.cmp(&b.datetime))
            }),
            (SortField::Tokens, SortDirection::Descending) => hourly.sort_by(|a, b| {
                b.tokens
                    .total()
                    .cmp(&a.tokens.total())
                    .then_with(|| a.datetime.cmp(&b.datetime))
            }),
            (SortField::Tokens, SortDirection::Ascending) => hourly.sort_by(|a, b| {
                a.tokens
                    .total()
                    .cmp(&b.tokens.total())
                    .then_with(|| a.datetime.cmp(&b.datetime))
            }),
            (SortField::Date, SortDirection::Descending) => {
                hourly.sort_by_key(|b| std::cmp::Reverse(b.datetime))
            }
            (SortField::Date, SortDirection::Ascending) => hourly.sort_by_key(|a| a.datetime),
        }

        hourly
    }

    pub fn get_sorted_periods(&self, kind: PeriodKind) -> Vec<PeriodUsage> {
        let mut periods = build_period_usage(&self.data.daily, kind);

        // Metric sorts keep Year sections newest-first; ordering is metric-based within each year.
        match (self.sort_field, self.sort_direction) {
            (SortField::Cost, SortDirection::Descending) => periods.sort_by(|a, b| {
                b.section_year
                    .cmp(&a.section_year)
                    .then_with(|| b.cost.total_cmp(&a.cost))
                    .then_with(|| b.start_date.cmp(&a.start_date))
            }),
            (SortField::Cost, SortDirection::Ascending) => periods.sort_by(|a, b| {
                b.section_year
                    .cmp(&a.section_year)
                    .then_with(|| a.cost.total_cmp(&b.cost))
                    .then_with(|| b.start_date.cmp(&a.start_date))
            }),
            (SortField::Tokens, SortDirection::Descending) => periods.sort_by(|a, b| {
                b.section_year
                    .cmp(&a.section_year)
                    .then_with(|| b.tokens.total().cmp(&a.tokens.total()))
                    .then_with(|| b.start_date.cmp(&a.start_date))
            }),
            (SortField::Tokens, SortDirection::Ascending) => periods.sort_by(|a, b| {
                b.section_year
                    .cmp(&a.section_year)
                    .then_with(|| a.tokens.total().cmp(&b.tokens.total()))
                    .then_with(|| b.start_date.cmp(&a.start_date))
            }),
            (SortField::Date, SortDirection::Descending) => {
                periods.sort_by_key(|period| std::cmp::Reverse(period.start_date))
            }
            (SortField::Date, SortDirection::Ascending) => {
                periods.sort_by_key(|period| period.start_date)
            }
        }

        periods
    }

    pub fn is_narrow(&self) -> bool {
        self.terminal_width < 80
    }

    pub fn is_very_narrow(&self) -> bool {
        self.terminal_width < 60
    }
}

#[cfg(test)]
mod tests {
    use super::super::ui::widgets::get_provider_shade;
    use super::*;
    use crate::tui::data::{DailyModelInfo, DailySourceInfo, ModelUsage, TokenBreakdown};
    use chrono::NaiveDate;
    use std::collections::BTreeMap;

    type SourceModelCosts<'a> = Vec<(&'a str, Vec<(&'a str, &'a str, f64)>)>;

    #[test]
    fn test_tab_all() {
        let tabs = Tab::all();
        assert_eq!(tabs.len(), 9);
        assert_eq!(tabs[0], Tab::Overview);
        assert_eq!(tabs[1], Tab::Usage);
        assert_eq!(tabs[2], Tab::Models);
        assert_eq!(tabs[3], Tab::Monthly);
        assert_eq!(tabs[4], Tab::Weekly);
        assert_eq!(tabs[5], Tab::Daily);
        assert_eq!(tabs[6], Tab::Hourly);
        assert_eq!(tabs[7], Tab::Stats);
        assert_eq!(tabs[8], Tab::Agents);
    }

    #[test]
    fn test_tab_next() {
        assert_eq!(Tab::Overview.next(), Tab::Usage);
        assert_eq!(Tab::Usage.next(), Tab::Models);
        assert_eq!(Tab::Models.next(), Tab::Monthly);
        assert_eq!(Tab::Monthly.next(), Tab::Weekly);
        assert_eq!(Tab::Weekly.next(), Tab::Daily);
        assert_eq!(Tab::Daily.next(), Tab::Hourly);
        assert_eq!(Tab::Hourly.next(), Tab::Stats);
        assert_eq!(Tab::Stats.next(), Tab::Agents);
        assert_eq!(Tab::Agents.next(), Tab::Overview);
    }

    #[test]
    fn test_tab_prev() {
        assert_eq!(Tab::Overview.prev(), Tab::Agents);
        assert_eq!(Tab::Usage.prev(), Tab::Overview);
        assert_eq!(Tab::Models.prev(), Tab::Usage);
        assert_eq!(Tab::Monthly.prev(), Tab::Models);
        assert_eq!(Tab::Weekly.prev(), Tab::Monthly);
        assert_eq!(Tab::Daily.prev(), Tab::Weekly);
        assert_eq!(Tab::Hourly.prev(), Tab::Daily);
        assert_eq!(Tab::Stats.prev(), Tab::Hourly);
        assert_eq!(Tab::Agents.prev(), Tab::Stats);
    }

    #[test]
    fn test_tab_as_str() {
        assert_eq!(Tab::Overview.as_str(), "Overview");
        assert_eq!(Tab::Models.as_str(), "Models");
        assert_eq!(Tab::Agents.as_str(), "Agents");
        assert_eq!(Tab::Monthly.as_str(), "Monthly");
        assert_eq!(Tab::Weekly.as_str(), "Weekly");
        assert_eq!(Tab::Daily.as_str(), "Daily");
        assert_eq!(Tab::Hourly.as_str(), "Hourly");
        assert_eq!(Tab::Stats.as_str(), "Stats");
    }

    #[test]
    fn test_tab_short_name() {
        assert_eq!(Tab::Overview.short_name(), "Ovw");
        assert_eq!(Tab::Models.short_name(), "Mod");
        assert_eq!(Tab::Agents.short_name(), "Agt");
        assert_eq!(Tab::Monthly.short_name(), "Mth");
        assert_eq!(Tab::Weekly.short_name(), "Wk");
        assert_eq!(Tab::Daily.short_name(), "Day");
        assert_eq!(Tab::Hourly.short_name(), "Hr");
        assert_eq!(Tab::Stats.short_name(), "Sta");
    }

    #[test]
    fn test_reset_selection() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();

        app.selected_index = 5;
        app.scroll_offset = 3;
        app.selected_graph_cell = Some((2, 4));

        app.reset_selection();

        assert_eq!(app.selected_index, 0);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.selected_graph_cell, None);
    }

    #[test]
    fn test_move_selection_up() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();

        // Add some mock data
        app.data.models = vec![
            ModelUsage {
                model: "model1".to_string(),
                provider: "provider1".to_string(),
                client: "opencode".to_string(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                performance: Default::default(),
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
            ModelUsage {
                model: "model2".to_string(),
                provider: "provider2".to_string(),
                client: "opencode".to_string(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                performance: Default::default(),
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
        ];

        app.selected_index = 1;
        app.move_selection_up();
        assert_eq!(app.selected_index, 0);

        // At top boundary - wraps to last item (index 1)
        app.move_selection_up();
        assert_eq!(app.selected_index, 1);
    }

    #[test]
    fn test_move_selection_down() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();

        // Add some mock data
        app.data.models = vec![
            ModelUsage {
                model: "model1".to_string(),
                provider: "provider1".to_string(),
                client: "opencode".to_string(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                performance: Default::default(),
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
            ModelUsage {
                model: "model2".to_string(),
                provider: "provider2".to_string(),
                client: "opencode".to_string(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                performance: Default::default(),
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
        ];

        app.selected_index = 0;
        app.move_selection_down();
        assert_eq!(app.selected_index, 1);

        // At bottom boundary - wraps to first item (index 0)
        app.move_selection_down();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn test_clamp_selection() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();

        // Add some mock data
        app.data.models = vec![ModelUsage {
            model: "model1".to_string(),
            provider: "provider1".to_string(),
            client: "opencode".to_string(),
            tokens: TokenBreakdown::default(),
            cost: 0.0,
            performance: Default::default(),
            session_count: 1,
            workspace_key: None,
            workspace_label: None,
        }];

        // Set selection beyond bounds
        app.selected_index = 10;
        app.clamp_selection();
        assert_eq!(app.selected_index, 0);

        // Empty data
        app.data.models.clear();
        app.selected_index = 5;
        app.clamp_selection();
        assert_eq!(app.selected_index, 0);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_set_sort() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();

        // Initial state
        assert_eq!(app.sort_field, SortField::Cost);
        assert_eq!(app.sort_direction, SortDirection::Descending);

        // Change to different field
        app.set_sort(SortField::Tokens);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);

        // Toggle same field
        app.set_sort(SortField::Tokens);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Ascending);

        // Toggle again
        app.set_sort(SortField::Tokens);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_should_quit() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let app = App::new_with_cached_data(config, None).unwrap();

        assert!(!app.should_quit);
    }

    // ── Helper ──────────────────────────────────────────────────────

    fn test_settings() -> Settings {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        drop(file);

        Settings::default().with_save_path_override(path)
    }

    fn make_app() -> App {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        App::new_with_cached_data_and_settings(config, None, test_settings()).unwrap()
    }

    fn make_app_with_usage() -> App {
        let mut settings = test_settings();
        settings.usage_tab_enabled = true;
        make_app_with_settings(settings)
    }

    fn make_app_with_usage_providers(providers: &[&str]) -> App {
        let mut settings = test_settings();
        settings.usage_tab_enabled = true;
        settings.usage_providers = providers
            .iter()
            .map(|provider| provider.to_string())
            .collect();
        make_app_with_settings(settings)
    }

    fn make_app_with_settings(settings: Settings) -> App {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        App::new_with_cached_data_and_settings(config, None, settings).unwrap()
    }

    #[test]
    fn test_app_no_filter_default_matches_default_set() {
        // Regression for an Oracle-flagged HIGH bug: the no-filter TUI
        // default and the `submit` warm-cache filter set drifted apart,
        // making every TUI launch after submit a stale-cache reuse
        // instead of a fresh hit. Both paths now go through
        // `ClientId::iter().collect()`; assert it stays that way.
        let app = make_app();
        let actual = app.enabled_clients.borrow().clone();
        let expected: HashSet<ClientId> = ClientId::iter().collect();
        assert_eq!(
            actual, expected,
            "no-filter App default drifted from ClientId::iter() — \
             warm cache and TUI launch will mismatch"
        );
    }

    fn make_app_with_models(n: usize) -> App {
        let mut app = make_app();
        app.data.models = (0..n)
            .map(|i| ModelUsage {
                model: format!("model{}", i),
                provider: "provider".to_string(),
                client: "opencode".to_string(),
                tokens: TokenBreakdown::default(),
                cost: 0.0,
                performance: Default::default(),
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            })
            .collect();
        app
    }

    fn daily_usage(date: &str, cost: f64, models: Vec<(&str, &str, f64)>) -> DailyUsage {
        daily_usage_by_source(date, cost, vec![("claude", models)])
    }

    fn daily_usage_by_source(date: &str, cost: f64, sources: SourceModelCosts<'_>) -> DailyUsage {
        let mut source_breakdown = BTreeMap::new();
        let mut total_tokens = TokenBreakdown::default();
        let mut total_cost = 0.0;

        for (source, models) in sources {
            let mut model_breakdown = BTreeMap::new();
            let mut source_tokens = TokenBreakdown::default();
            let mut source_cost = 0.0;

            for (model, provider, model_cost) in models {
                let tokens = TokenBreakdown {
                    input: (model_cost * 100.0) as u64,
                    output: 10,
                    cache_read: 5,
                    cache_write: 0,
                    reasoning: 0,
                };
                source_tokens.input = source_tokens.input.saturating_add(tokens.input);
                source_tokens.output = source_tokens.output.saturating_add(tokens.output);
                source_tokens.cache_read =
                    source_tokens.cache_read.saturating_add(tokens.cache_read);
                total_tokens.input = total_tokens.input.saturating_add(tokens.input);
                total_tokens.output = total_tokens.output.saturating_add(tokens.output);
                total_tokens.cache_read = total_tokens.cache_read.saturating_add(tokens.cache_read);
                source_cost += model_cost;
                total_cost += model_cost;

                model_breakdown.insert(
                    model.to_string(),
                    DailyModelInfo {
                        provider: provider.to_string(),
                        display_name: model.to_string(),
                        color_key: model.to_string(),
                        tokens,
                        cost: model_cost,
                        messages: 1,
                    },
                );
            }

            source_breakdown.insert(
                source.to_string(),
                DailySourceInfo {
                    tokens: source_tokens,
                    cost: source_cost,
                    models: model_breakdown,
                },
            );
        }

        DailyUsage {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            tokens: total_tokens,
            cost: if cost > 0.0 { cost } else { total_cost },
            source_breakdown,
            message_count: 1,
            turn_count: 1,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_with_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    // ── handle_key_event: quit ──────────────────────────────────────

    #[test]
    fn test_handle_key_quit_q() {
        let mut app = make_app();
        let quit = app.handle_key_event(key(KeyCode::Char('q')));
        assert!(quit);
        assert!(app.should_quit);
    }

    #[test]
    fn test_handle_key_quit_ctrl_c() {
        let mut app = make_app();
        let quit = app.handle_key_event(key_with_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(quit);
        assert!(app.should_quit);
    }

    #[test]
    fn test_dialog_ctrl_c_still_global_quit() {
        let mut app = make_app();
        app.open_client_picker();

        let quit = app.handle_key_event(key_with_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(quit);
        assert!(app.should_quit);
        assert!(!*app.dialog_needs_reload.borrow());
    }

    // ── handle_key_event: tab switching ─────────────────────────────

    #[test]
    fn test_handle_key_tab_switch() {
        let mut app = make_app();
        assert_eq!(app.current_tab, Tab::Overview);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Models);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Monthly);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Weekly);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Daily);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Hourly);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Stats);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Agents);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn test_handle_key_backtab_switch() {
        let mut app = make_app();
        assert_eq!(app.current_tab, Tab::Overview);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Agents);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Stats);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Hourly);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Daily);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Weekly);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Monthly);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Models);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn test_handle_key_tab_switch_with_usage_enabled_includes_usage() {
        let mut app = make_app_with_usage();
        assert_eq!(app.current_tab, Tab::Overview);

        for expected in [
            Tab::Usage,
            Tab::Models,
            Tab::Monthly,
            Tab::Weekly,
            Tab::Daily,
            Tab::Hourly,
            Tab::Stats,
            Tab::Agents,
            Tab::Overview,
        ] {
            app.handle_key_event(key(KeyCode::Tab));
            assert_eq!(app.current_tab, expected);
        }
    }

    #[test]
    fn test_initial_usage_tab_clamps_to_overview_when_flag_off() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: Some(Tab::Usage),
        };
        let app = App::new_with_cached_data_and_settings(
            config,
            Some(UsageData::default()),
            Settings::default(),
        )
        .unwrap();
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn test_get_sorted_agents_by_cost_desc() {
        let mut app = make_app();
        app.data.agents = vec![
            AgentUsage {
                agent: "builder".to_string(),
                clients: "opencode".to_string(),
                tokens: TokenBreakdown {
                    input: 10,
                    output: 5,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: 3.0,
                message_count: 1,
                instance_count: 1,
            },
            AgentUsage {
                agent: "reviewer".to_string(),
                clients: "roocode".to_string(),
                tokens: TokenBreakdown {
                    input: 50,
                    output: 20,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: 7.0,
                message_count: 2,
                instance_count: 2,
            },
        ];

        let agents = app.get_sorted_agents();
        assert_eq!(agents[0].agent, "reviewer");
        assert_eq!(agents[1].agent, "builder");
    }

    #[test]
    fn test_get_sorted_agents_by_tokens_asc() {
        let mut app = make_app();
        app.sort_field = SortField::Tokens;
        app.sort_direction = SortDirection::Ascending;
        app.data.agents = vec![
            AgentUsage {
                agent: "builder".to_string(),
                clients: "opencode".to_string(),
                tokens: TokenBreakdown {
                    input: 100,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: 1.0,
                message_count: 1,
                instance_count: 1,
            },
            AgentUsage {
                agent: "reviewer".to_string(),
                clients: "roocode".to_string(),
                tokens: TokenBreakdown {
                    input: 20,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: 5.0,
                message_count: 1,
                instance_count: 1,
            },
        ];

        let agents = app.get_sorted_agents();
        assert_eq!(agents[0].agent, "reviewer");
        assert_eq!(agents[1].agent, "builder");
    }

    #[test]
    fn test_handle_key_left_right_switch() {
        let mut app = make_app();
        app.handle_key_event(key(KeyCode::Right));
        assert_eq!(app.current_tab, Tab::Models);

        app.handle_key_event(key(KeyCode::Left));
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn test_handle_key_left_right_switch_with_usage_enabled() {
        let mut app = make_app_with_usage();
        app.handle_key_event(key(KeyCode::Right));
        assert_eq!(app.current_tab, Tab::Usage);

        app.handle_key_event(key(KeyCode::Right));
        assert_eq!(app.current_tab, Tab::Models);

        app.handle_key_event(key(KeyCode::Left));
        assert_eq!(app.current_tab, Tab::Usage);
    }

    #[test]
    fn test_handle_key_tab_resets_selection() {
        let mut app = make_app_with_models(5);
        app.selected_index = 3;
        app.scroll_offset = 1;
        app.selected_graph_cell = Some((2, 4));

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.selected_index, 0);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.selected_graph_cell, None);
    }

    #[test]
    fn test_enter_on_daily_opens_selected_day_detail_rows() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
            daily_usage(
                "2026-05-17",
                7.0,
                vec![("target-a", "openai", 5.0), ("target-b", "anthropic", 2.0)],
            ),
            daily_usage("2026-05-18", 3.0, vec![("other-model", "google", 3.0)]),
        ];

        app.selected_index = 0;
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(app.get_current_list_len(), 2);
    }

    #[test]
    fn test_enter_on_daily_detail_uses_token_sort_default() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![daily_usage(
            "2026-05-17",
            8.0,
            vec![
                ("a-low-token", "anthropic", 1.0),
                ("z-high-token", "openai", 7.0),
            ],
        )];

        app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
        assert_eq!(app.get_sorted_daily_detail_rows()[0].model, "z-high-token");
    }

    #[test]
    fn test_esc_from_daily_detail_restores_daily_selection() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
            daily_usage(
                "2026-05-17",
                7.0,
                vec![("target-a", "openai", 5.0), ("target-b", "anthropic", 2.0)],
            ),
            daily_usage("2026-05-18", 3.0, vec![("other-model", "google", 3.0)]),
        ];

        app.max_visible_items = 2;
        app.selected_index = 1;
        app.scroll_offset = 1;
        app.handle_key_event(key(KeyCode::Enter));
        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);

        app.handle_key_event(key(KeyCode::Esc));

        assert_eq!(app.current_tab, Tab::Daily);
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.scroll_offset, 1);
        assert_eq!(app.get_current_list_len(), 3);
    }

    #[test]
    fn test_close_daily_detail_reanchors_selection_by_date_after_sort_change() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
            daily_usage(
                "2026-05-17",
                7.0,
                vec![("target-a", "openai", 5.0), ("target-b", "anthropic", 2.0)],
            ),
            daily_usage("2026-05-18", 3.0, vec![("other-model", "google", 3.0)]),
        ];

        app.selected_index = 1;
        let target_date = app.get_sorted_daily()[app.selected_index].date;

        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_daily_detail_active());
        assert_eq!(app.daily_detail_date(), Some(target_date));

        app.handle_key_event(key(KeyCode::Char('c')));
        assert_eq!(app.sort_field, SortField::Cost);

        app.handle_key_event(key(KeyCode::Esc));

        assert!(!app.is_daily_detail_active());
        let restored_index = app.selected_index;
        let restored_date = app.get_sorted_daily()[restored_index].date;
        assert_eq!(
            restored_date, target_date,
            "Closing detail after sort change should re-anchor on the original date"
        );
    }

    #[test]
    fn test_update_data_exits_daily_detail_when_date_disappears() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
            daily_usage(
                "2026-05-17",
                7.0,
                vec![("target-a", "openai", 5.0), ("target-b", "anthropic", 2.0)],
            ),
            daily_usage("2026-05-18", 3.0, vec![("other-model", "google", 3.0)]),
        ];

        app.selected_index = 1;
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_daily_detail_active());

        let refreshed = UsageData {
            daily: vec![
                daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
                daily_usage("2026-05-18", 3.0, vec![("other-model", "google", 3.0)]),
            ],
            ..Default::default()
        };
        app.update_data(refreshed);

        assert!(
            !app.is_daily_detail_active(),
            "update_data should drop detail mode when the selected date is gone"
        );
        assert_eq!(app.daily_detail_date(), None);
        assert!(app.get_sorted_daily_detail_rows().is_empty());
    }

    #[test]
    fn test_update_data_keeps_daily_detail_when_date_still_present() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
            daily_usage(
                "2026-05-17",
                7.0,
                vec![("target-a", "openai", 5.0), ("target-b", "anthropic", 2.0)],
            ),
        ];

        app.selected_index = 1;
        let target_date = app.get_sorted_daily()[app.selected_index].date;
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_daily_detail_active());

        let refreshed = UsageData {
            daily: vec![
                daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
                daily_usage(
                    "2026-05-17",
                    9.0,
                    vec![("target-a", "openai", 7.0), ("target-b", "anthropic", 2.0)],
                ),
            ],
            ..Default::default()
        };
        app.update_data(refreshed);

        assert!(app.is_daily_detail_active());
        assert_eq!(app.daily_detail_date(), Some(target_date));
    }

    #[test]
    fn test_daily_detail_updates_rows_after_group_by_reload() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        *app.group_by.borrow_mut() = tokscale_core::GroupBy::ClientModel;
        app.data.daily = vec![daily_usage_by_source(
            "2026-05-17",
            0.0,
            vec![
                ("claude", vec![("claude:gpt-5", "openai", 5.0)]),
                ("codex", vec![("codex:gpt-5", "openai", 2.0)]),
            ],
        )];

        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_daily_detail_active());
        assert_eq!(app.get_sorted_daily_detail_rows().len(), 2);

        *app.group_by.borrow_mut() = tokscale_core::GroupBy::Model;
        app.update_data(UsageData {
            daily: vec![daily_usage_by_source(
                "2026-05-17",
                0.0,
                vec![
                    ("claude", vec![("gpt-5", "openai", 5.0)]),
                    ("codex", vec![("gpt-5", "openai", 2.0)]),
                ],
            )],
            ..Default::default()
        });

        let rows = app.get_sorted_daily_detail_rows();
        assert!(app.is_daily_detail_active());
        assert_eq!(
            app.daily_detail_date(),
            Some(NaiveDate::from_ymd_opt(2026, 5, 17).unwrap())
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "claude, codex");
        assert_eq!(rows[0].model, "gpt-5");
        assert_eq!(rows[0].tokens.total(), 730);
        assert_eq!(rows[0].messages, 2);
        assert!((rows[0].cost - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_enter_on_monthly_opens_selected_period_detail_rows() {
        let mut app = make_app();
        app.current_tab = Tab::Monthly;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            daily_usage("2026-05-17", 7.0, vec![("target-a", "openai", 5.0)]),
            daily_usage(
                "2026-05-18",
                5.0,
                vec![("target-a", "openai", 3.0), ("target-b", "anthropic", 2.0)],
            ),
            daily_usage("2026-06-01", 1.0, vec![("june-model", "google", 1.0)]),
        ];

        app.selected_index = 1;
        let selected_period =
            app.get_sorted_periods(PeriodKind::Monthly)[app.selected_index].start_date;
        app.handle_key_event(key(KeyCode::Enter));

        assert!(app.is_period_detail_active_for_kind(PeriodKind::Monthly));
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
        assert_eq!(app.get_current_list_len(), 2);
        assert_eq!(app.get_sorted_period_detail_rows()[0].model, "target-a");
        assert_eq!(
            app.selected_period_detail.unwrap().start_date,
            selected_period
        );
    }

    #[test]
    fn test_period_detail_model_name_falls_back_to_model_key() {
        let mut app = make_app();
        app.current_tab = Tab::Monthly;
        app.data.daily = vec![daily_usage(
            "2026-05-17",
            7.0,
            vec![("fallback-model", "openai", 7.0)],
        )];
        app.data.daily[0]
            .source_breakdown
            .get_mut("claude")
            .unwrap()
            .models
            .get_mut("fallback-model")
            .unwrap()
            .display_name
            .clear();

        app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(
            app.get_sorted_period_detail_rows()[0].model,
            "fallback-model"
        );
    }

    #[test]
    fn test_esc_from_weekly_detail_restores_week_selection() {
        let mut app = make_app();
        app.current_tab = Tab::Weekly;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            daily_usage("2026-06-01", 1.0, vec![("week-23-a", "openai", 1.0)]),
            daily_usage("2026-06-03", 2.0, vec![("week-23-b", "anthropic", 2.0)]),
            daily_usage("2026-06-10", 3.0, vec![("week-24", "google", 3.0)]),
            daily_usage("2026-06-17", 4.0, vec![("week-25", "google", 4.0)]),
        ];

        app.max_visible_items = 2;
        app.selected_index = 2;
        app.scroll_offset = 1;
        let selected_period =
            app.get_sorted_periods(PeriodKind::Weekly)[app.selected_index].start_date;

        app.handle_key_event(key(KeyCode::Enter));
        app.handle_key_event(key(KeyCode::Down));
        assert!(app.is_period_detail_active_for_kind(PeriodKind::Weekly));
        assert_eq!(app.selected_index, 1);

        app.handle_key_event(key(KeyCode::Esc));

        assert!(!app.is_period_detail_active());
        assert_eq!(app.current_tab, Tab::Weekly);
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);
        assert_eq!(app.selected_index, 2);
        assert_eq!(app.scroll_offset, 1);
        assert_eq!(
            app.get_sorted_periods(PeriodKind::Weekly)[app.selected_index].start_date,
            selected_period
        );
    }

    #[test]
    fn test_update_data_exits_period_detail_when_period_disappears() {
        let mut app = make_app();
        app.current_tab = Tab::Weekly;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.data.daily = vec![
            daily_usage("2026-06-01", 1.0, vec![("target", "openai", 1.0)]),
            daily_usage("2026-06-10", 3.0, vec![("other", "google", 3.0)]),
        ];

        app.selected_index = 1;
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_period_detail_active_for_kind(PeriodKind::Weekly));

        let refreshed = UsageData {
            daily: vec![daily_usage(
                "2026-06-10",
                3.0,
                vec![("other", "google", 3.0)],
            )],
            ..Default::default()
        };
        app.update_data(refreshed);

        assert!(
            !app.is_period_detail_active(),
            "update_data should drop period detail mode when the selected period is gone"
        );
        assert!(app.get_sorted_period_detail_rows().is_empty());
    }

    #[test]
    fn test_period_detail_uses_grouped_detail_rows() {
        let mut app = make_app();
        app.current_tab = Tab::Monthly;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        *app.group_by.borrow_mut() = tokscale_core::GroupBy::Model;
        app.data.daily = vec![
            daily_usage_by_source(
                "2026-05-17",
                0.0,
                vec![
                    ("claude", vec![("gpt-5", "openai", 5.0)]),
                    ("codex", vec![("gpt-5", "openai", 2.0)]),
                ],
            ),
            daily_usage("2026-06-01", 1.0, vec![("june-model", "google", 1.0)]),
        ];

        app.selected_index = 1;
        app.handle_key_event(key(KeyCode::Enter));

        let rows = app.get_sorted_period_detail_rows();
        assert!(app.is_period_detail_active_for_kind(PeriodKind::Monthly));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "claude, codex");
        assert_eq!(rows[0].model, "gpt-5");
        assert_eq!(rows[0].tokens.total(), 730);
        assert_eq!(rows[0].messages, 2);
        assert!((rows[0].cost - 7.0).abs() < f64::EPSILON);
    }

    // ── handle_key_event: sort ──────────────────────────────────────

    #[test]
    fn test_handle_key_sort_cost() {
        let mut app = make_app();
        app.handle_key_event(key(KeyCode::Char('c')));
        assert_eq!(app.sort_field, SortField::Cost);
        assert_eq!(app.sort_direction, SortDirection::Ascending);
    }

    #[test]
    fn test_handle_key_sort_tokens() {
        let mut app = make_app();
        app.handle_key_event(key(KeyCode::Char('t')));
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_handle_key_sort_date() {
        let mut app = make_app();
        app.handle_key_event(key(KeyCode::Char('d')));
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_handle_key_sort_toggle_direction() {
        let mut app = make_app();
        app.handle_key_event(key(KeyCode::Char('t')));
        assert_eq!(app.sort_direction, SortDirection::Descending);

        app.handle_key_event(key(KeyCode::Char('t')));
        assert_eq!(app.sort_direction, SortDirection::Ascending);

        app.handle_key_event(key(KeyCode::Char('t')));
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_switch_tab_restores_hourly_date_default() {
        let mut app = make_app();
        assert_eq!(app.sort_field, SortField::Cost);

        app.switch_tab(Tab::Hourly);
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);

        app.switch_tab(Tab::Models);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_initial_models_tab_uses_token_sort_default() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: Some(Tab::Models),
        };

        let app =
            App::new_with_cached_data_and_settings(config, None, Settings::default()).unwrap();

        assert_eq!(app.current_tab, Tab::Models);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_models_default_sort_shows_highest_tokens_first() {
        let mut app = make_app();
        app.data.models = vec![
            ModelUsage {
                model: "expensive-low-token".to_string(),
                provider: "anthropic".to_string(),
                client: "claude".to_string(),
                tokens: TokenBreakdown {
                    input: 10,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: 100.0,
                performance: Default::default(),
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
            ModelUsage {
                model: "cheap-high-token".to_string(),
                provider: "anthropic".to_string(),
                client: "claude".to_string(),
                tokens: TokenBreakdown {
                    input: 1_000,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: 1.0,
                performance: Default::default(),
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
        ];

        app.switch_tab(Tab::Models);

        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
        assert_eq!(app.get_sorted_models()[0].model, "cheap-high-token");
    }

    #[test]
    fn test_initial_hourly_tab_uses_hourly_sort_default() {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: Some(Tab::Hourly),
        };

        let app = App::new_with_cached_data(config, None).unwrap();

        assert_eq!(app.current_tab, Tab::Hourly);
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_switch_tab_uses_daily_date_default() {
        let mut app = make_app();

        app.switch_tab(Tab::Daily);

        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_daily_default_sort_shows_latest_date_first() {
        let mut app = make_app();
        app.data.daily = vec![
            daily_usage(
                "2026-05-24",
                99.0,
                vec![("older-expensive", "anthropic", 99.0)],
            ),
            daily_usage("2026-05-26", 1.0, vec![("newer-cheap", "anthropic", 1.0)]),
            daily_usage("2026-05-25", 50.0, vec![("middle", "anthropic", 50.0)]),
        ];

        app.switch_tab(Tab::Daily);

        let dates = app
            .get_sorted_daily()
            .iter()
            .map(|entry| entry.date)
            .collect::<Vec<_>>();
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 5, 26).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 25).unwrap(),
                NaiveDate::from_ymd_opt(2026, 5, 24).unwrap(),
            ]
        );
    }

    #[test]
    fn test_switch_tab_preserves_user_sort() {
        let mut app = make_app();
        app.switch_tab(Tab::Models);

        app.set_sort(SortField::Cost);
        assert_eq!(app.sort_field, SortField::Cost);
        assert_eq!(app.sort_direction, SortDirection::Descending);

        app.switch_tab(Tab::Daily);
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);

        app.switch_tab(Tab::Models);
        assert_eq!(app.sort_field, SortField::Cost);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_switch_tab_preserves_daily_sort_after_hourly_roundtrip() {
        let mut app = make_app();

        app.switch_tab(Tab::Daily);
        app.set_sort(SortField::Tokens);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);

        app.switch_tab(Tab::Hourly);
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);

        app.switch_tab(Tab::Daily);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    // ── handle_key_event: navigation ────────────────────────────────

    #[test]
    fn test_handle_key_navigation_up_down() {
        let mut app = make_app_with_models(5);
        assert_eq!(app.selected_index, 0);

        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(app.selected_index, 1);

        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(app.selected_index, 2);

        app.handle_key_event(key(KeyCode::Up));
        assert_eq!(app.selected_index, 1);

        app.handle_key_event(key(KeyCode::Up));
        assert_eq!(app.selected_index, 0);

        // At top boundary - wraps to last item (index 4, 5 models)
        app.handle_key_event(key(KeyCode::Up));
        assert_eq!(app.selected_index, 4);
    }

    #[test]
    fn test_handle_key_navigation_boundary() {
        let mut app = make_app_with_models(3);
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(app.selected_index, 2);

        // At bottom boundary - wraps to first item (index 0)
        app.handle_key_event(key(KeyCode::Down));
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn usage_tab_key_scrolls_text_viewport_without_table_selection() {
        let mut app = make_app_with_usage();
        app.current_tab = Tab::Usage;
        app.selected_index = 3;
        app.scroll_offset = 2;
        app.set_usage_text_viewport(4, 10);

        app.handle_key_event(key(KeyCode::Down));

        assert_eq!(app.usage_viewport.scroll, 1);
        assert_eq!(app.selected_index, 3);
        assert_eq!(app.scroll_offset, 2);
    }

    #[test]
    fn hourly_profile_key_scrolls_text_viewport_without_table_selection() {
        let mut app = make_app();
        app.current_tab = Tab::Hourly;
        app.hourly_view_mode = HourlyViewMode::Profile;
        app.selected_index = 2;
        app.scroll_offset = 1;
        app.set_hourly_profile_text_viewport(4, 10);

        app.handle_key_event(key(KeyCode::PageDown));

        assert_eq!(app.hourly_profile_viewport.scroll, 2);
        assert_eq!(app.selected_index, 2);
        assert_eq!(app.scroll_offset, 1);
    }

    #[test]
    fn usage_tab_mouse_wheel_scrolls_text_viewport_without_table_selection() {
        let mut app = make_app_with_usage();
        app.current_tab = Tab::Usage;
        app.selected_index = 4;
        app.scroll_offset = 3;
        app.set_usage_text_viewport(4, 10);

        app.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(app.usage_viewport.scroll, 1);
        assert_eq!(app.selected_index, 4);
        assert_eq!(app.scroll_offset, 3);
    }

    // ── wrap-around navigation ──────────────────────────────────────

    #[test]
    fn test_move_selection_up_wraps_to_last() {
        let mut app = make_app_with_models(3);
        app.max_visible_items = 10;
        app.selected_index = 0;
        app.move_selection_up();
        assert_eq!(app.selected_index, 2);
    }

    #[test]
    fn test_move_selection_down_wraps_to_first() {
        let mut app = make_app_with_models(3);
        app.max_visible_items = 10;
        app.selected_index = 2;
        app.move_selection_down();
        assert_eq!(app.selected_index, 0);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_move_selection_up_empty_list_noop() {
        let mut app = make_app();
        app.data.models.clear();
        app.selected_index = 0;
        app.move_selection_up();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn test_move_selection_down_empty_list_noop() {
        let mut app = make_app();
        app.data.models.clear();
        app.selected_index = 0;
        app.move_selection_down();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn test_move_selection_up_wrap_scroll_offset() {
        let mut app = make_app_with_models(10);
        app.max_visible_items = 3;
        app.selected_index = 0;
        app.move_selection_up();
        // Should wrap to index 9 and scroll so last item is visible
        assert_eq!(app.selected_index, 9);
        assert_eq!(app.scroll_offset, 7); // 10 - 3 = 7
    }

    #[test]
    fn test_move_selection_down_wrap_resets_scroll() {
        let mut app = make_app_with_models(10);
        app.max_visible_items = 3;
        app.selected_index = 9;
        app.scroll_offset = 7;
        app.move_selection_down();
        assert_eq!(app.selected_index, 0);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_overview_scroll_keeps_rendered_capacity_after_resize() {
        let mut app = make_app_with_models(33);
        app.current_tab = Tab::Overview;
        app.set_max_visible_items(9);

        for _ in 0..32 {
            app.move_selection_down();
            app.handle_resize(120, 40);
            app.set_max_visible_items(9);
        }

        assert_eq!(app.selected_index, 32);
        assert_eq!(app.scroll_offset, 24);
    }

    // ── handle_key_event: theme ─────────────────────────────────────

    #[test]
    fn test_handle_key_theme_cycle() {
        let mut app = make_app();
        let initial_theme = app.theme.name;
        let settings_path = app.settings.save_path_override.clone().unwrap();

        app.handle_key_event(key(KeyCode::Char('p')));
        assert_ne!(app.theme.name, initial_theme);
        assert!(
            settings_path.exists(),
            "theme save must use the isolated test settings file"
        );

        for _ in 0..8 {
            app.handle_key_event(key(KeyCode::Char('p')));
        }
        assert_eq!(app.theme.name, initial_theme);
    }

    // ── handle_key_event: export ────────────────────────────────────

    #[test]
    fn test_handle_key_export() {
        let mut app = make_app();
        app.handle_key_event(key(KeyCode::Char('e')));
        assert!(app.status_message.is_some());
        let msg = app.status_message.as_ref().unwrap();
        assert!(
            msg.contains("Exported to") || msg.contains("Export failed"),
            "unexpected status: {}",
            msg
        );
    }

    // ── handle_key_event: refresh ───────────────────────────────────

    #[test]
    #[ignore] // triggers load_data() which requires network + filesystem I/O
    fn test_handle_key_refresh() {
        let mut app = make_app();
        std::thread::sleep(Duration::from_millis(5));
        app.handle_key_event(key(KeyCode::Char('r')));
        assert!(app.needs_reload);
    }

    #[test]
    fn test_handle_key_refresh_while_loading_does_not_queue_reload() {
        let mut app = make_app();
        app.background_loading = true;

        app.handle_key_event(key(KeyCode::Char('r')));

        assert!(!app.needs_reload);
        assert_eq!(
            app.status_message.as_deref(),
            Some("Refresh already in progress")
        );
    }

    #[test]
    fn test_group_by_change_requests_blocking_reload() {
        let mut app = make_app();
        *app.group_by.borrow_mut() = tokscale_core::GroupBy::ClientModel;

        app.handle_key_event(key(KeyCode::Char('g')));
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(
            *app.group_by.borrow(),
            tokscale_core::GroupBy::WorkspaceModel
        );
        assert!(!app.dialog_stack.is_active());
        assert!(app.needs_reload);
        assert!(app.blocking_loading);
        assert!(app.is_blocking_loading());
    }

    #[test]
    fn test_source_picker_waits_until_close_before_blocking_reload() {
        let mut app = make_app();

        app.handle_key_event(key(KeyCode::Char('s')));
        assert!(app.dialog_stack.is_active());

        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.dialog_stack.is_active());
        assert!(!app.needs_reload);
        assert!(!app.blocking_loading);
        assert!(!app.is_blocking_loading());

        app.on_tick();
        assert!(!app.needs_reload);
        assert!(!app.blocking_loading);

        app.handle_key_event(key(KeyCode::Esc));

        assert!(!app.dialog_stack.is_active());
        assert!(app.needs_reload);
        assert!(app.blocking_loading);
        assert!(app.is_blocking_loading());
    }

    #[test]
    fn test_background_loading_clear_resets_blocking_loading() {
        let mut app = make_app();

        app.request_blocking_reload();
        app.set_background_loading(true);
        app.set_background_loading(false);

        assert!(!app.background_loading);
        assert!(!app.blocking_loading);
    }

    // ── handle_key_event: misc keys ─────────────────────────────────

    #[test]
    fn test_handle_key_esc_clears_graph_selection() {
        let mut app = make_app();
        app.selected_graph_cell = Some((1, 2));

        app.handle_key_event(key(KeyCode::Esc));
        assert_eq!(app.selected_graph_cell, None);
    }

    #[test]
    fn test_handle_key_enter_on_stats() {
        let mut app = make_app();
        app.current_tab = Tab::Stats;
        app.selected_graph_cell = Some((1, 2));

        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.status_message.is_some());
    }

    #[test]
    fn test_handle_key_unrecognized_returns_false() {
        let mut app = make_app();
        let result = app.handle_key_event(key(KeyCode::F(12)));
        assert!(!result);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_handle_key_auto_refresh_toggle() {
        let mut app = make_app();
        let initial = app.auto_refresh;
        app.handle_key_event(key_with_mod(KeyCode::Char('R'), KeyModifiers::SHIFT));
        assert_ne!(app.auto_refresh, initial);
    }

    #[test]
    fn test_usage_fetch_completion_updates_subscription_status_and_timestamp() {
        let mut app = make_app();
        let (_tx, rx) = std::sync::mpsc::channel();

        app.start_subscription_usage_fetch_for_test(rx);

        assert_eq!(
            app.status_message.as_deref(),
            Some("Fetching subscription usage...")
        );
        assert!(app.usage_fetch_attempted);
        assert!(app.is_fetching_usage());
    }

    #[test]
    fn test_usage_fetch_completion_sets_subscription_check_clock() {
        let mut app = make_app();
        let (tx, rx) = std::sync::mpsc::channel();
        app.start_subscription_usage_fetch_for_test(rx);
        tx.send(crate::commands::usage::UsageFetchBatch::default())
            .unwrap();

        app.on_tick();

        assert_eq!(
            app.status_message.as_deref(),
            Some("No usage data available")
        );
        assert!(app.last_subscription_usage_check.is_some());
        assert!(!app.is_fetching_usage());
    }

    #[test]
    fn test_fetch_subscription_usage_while_fetching_reports_in_progress() {
        let mut app = make_app();
        app.current_tab = Tab::Usage;
        let (_tx, rx) = std::sync::mpsc::channel();
        app.start_subscription_usage_fetch_for_test(rx);

        app.fetch_subscription_usage();

        assert_eq!(
            app.status_message.as_deref(),
            Some("Subscription usage fetch already in progress")
        );
        assert!(app.is_fetching_usage());
    }

    #[test]
    fn test_auto_refresh_on_overview_refreshes_token_data_only() {
        let mut app = make_app();
        app.current_tab = Tab::Overview;
        app.auto_refresh = true;
        app.auto_refresh_interval = Duration::from_millis(1);
        app.last_refresh = Instant::now() - Duration::from_secs(1);

        app.on_tick();

        assert!(app.needs_reload);
        assert!(!app.usage_fetch_attempted);
        assert!(!app.is_fetching_usage());
    }

    #[test]
    fn test_usage_tab_auto_refresh_does_not_fetch_subscription_usage() {
        let mut app = make_app();
        app.current_tab = Tab::Usage;
        app.auto_refresh = true;
        app.auto_refresh_interval = Duration::from_secs(60);
        let stale = Instant::now() - Duration::from_secs(120);
        app.last_refresh = stale;

        app.on_tick();

        assert!(!app.usage_fetch_attempted);
        assert!(!app.needs_reload);

        app.switch_tab(Tab::Overview);

        assert!(app.needs_reload);
    }

    #[test]
    fn test_switching_to_usage_without_enabled_providers_does_not_fetch() {
        let mut app = make_app_with_usage();
        app.current_tab = Tab::Overview;
        app.auto_refresh = true;
        app.auto_refresh_interval = Duration::from_secs(60);
        let stale = Instant::now() - Duration::from_secs(120);
        app.last_refresh = stale;

        app.on_tick();
        assert!(app.needs_reload);
        assert!(!app.usage_fetch_attempted);

        app.needs_reload = false;
        app.update_data(UsageData::default());
        app.switch_tab(Tab::Usage);

        assert!(!app.usage_fetch_attempted);
        assert!(!app.is_fetching_usage());
    }

    #[test]
    fn test_initial_usage_fetch_requires_enabled_provider() {
        let mut app = make_app_with_usage();
        app.current_tab = Tab::Usage;
        assert!(!app.should_start_initial_subscription_usage_fetch());

        let mut app = make_app_with_usage_providers(&["codex"]);
        app.current_tab = Tab::Usage;
        assert!(app.should_start_initial_subscription_usage_fetch());
    }

    #[test]
    fn test_initial_usage_fetch_starts_only_once_per_session() {
        let mut app = make_app_with_usage_providers(&["codex"]);
        app.current_tab = Tab::Overview;
        assert!(!app.should_start_initial_subscription_usage_fetch());

        app.current_tab = Tab::Usage;
        assert!(app.should_start_initial_subscription_usage_fetch());
        app.usage_initial_fetch_started = true;

        app.current_tab = Tab::Models;
        assert!(!app.should_start_initial_subscription_usage_fetch());

        app.current_tab = Tab::Usage;
        assert!(!app.should_start_initial_subscription_usage_fetch());
    }

    #[test]
    fn test_r_refreshes_local_report_only_with_enabled_usage_provider() {
        let mut app = make_app_with_usage_providers(&["codex"]);
        app.current_tab = Tab::Usage;

        app.handle_key_event(key(KeyCode::Char('r')));

        assert!(app.needs_reload);
        assert!(app.reload_force);
        assert!(!app.usage_fetch_attempted);
        assert!(!app.is_fetching_usage());
    }

    #[test]
    fn test_u_outside_usage_does_not_fetch_subscription_usage() {
        let mut app = make_app_with_usage_providers(&["codex"]);
        app.current_tab = Tab::Overview;

        app.handle_key_event(key(KeyCode::Char('u')));

        assert!(!app.usage_fetch_attempted);
        assert!(!app.is_fetching_usage());
    }

    #[test]
    fn test_u_on_usage_without_enabled_providers_reports_disabled() {
        let mut app = make_app_with_usage();
        app.current_tab = Tab::Usage;

        app.handle_key_event(key(KeyCode::Char('u')));

        assert!(!app.usage_fetch_attempted);
        assert_eq!(
            app.status_message.as_deref(),
            Some("No subscription usage providers enabled")
        );
    }

    #[test]
    fn test_enabling_auto_refresh_waits_for_next_interval() {
        let mut app = make_app();
        app.auto_refresh = false;
        app.auto_refresh_interval = Duration::from_secs(60);
        app.last_refresh = Instant::now() - Duration::from_secs(120);

        app.handle_key_event(key_with_mod(KeyCode::Char('R'), KeyModifiers::SHIFT));
        app.on_tick();

        assert!(app.auto_refresh);
        assert!(!app.needs_reload);
        assert!(!app.usage_fetch_attempted);
    }

    #[test]
    fn test_handle_key_increase_decrease_refresh() {
        let mut app = make_app();
        let initial_interval = app.auto_refresh_interval;

        app.handle_key_event(key(KeyCode::Char('+')));
        assert!(app.auto_refresh_interval > initial_interval);

        let after_increase = app.auto_refresh_interval;
        app.handle_key_event(key(KeyCode::Char('-')));
        assert!(app.auto_refresh_interval < after_increase);
    }

    // ── handle_mouse_event ──────────────────────────────────────────

    #[test]
    fn test_handle_mouse_left_click() {
        let mut app = make_app();
        app.add_click_area(Rect::new(0, 0, 10, 2), ClickAction::Tab(Tab::Models));

        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(event);
        assert_eq!(app.current_tab, Tab::Models);
    }

    #[test]
    fn test_handle_mouse_click_sort() {
        let mut app = make_app();
        app.add_click_area(Rect::new(0, 0, 10, 2), ClickAction::Sort(SortField::Tokens));

        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(event);
        assert_eq!(app.sort_field, SortField::Tokens);
    }

    #[test]
    fn test_handle_mouse_click_graph_cell() {
        let mut app = make_app();
        app.add_click_area(
            Rect::new(10, 5, 3, 3),
            ClickAction::GraphCell { week: 2, day: 3 },
        );

        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 11,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(event);
        assert_eq!(app.selected_graph_cell, Some((2, 3)));
    }

    #[test]
    fn test_handle_mouse_click_outside_areas() {
        let mut app = make_app();
        app.add_click_area(Rect::new(0, 0, 5, 5), ClickAction::Tab(Tab::Stats));

        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 50,
            row: 50,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(event);
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn test_handle_mouse_scroll_up() {
        let mut app = make_app_with_models(5);
        app.selected_index = 2;

        let event = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(event);
        assert_eq!(app.selected_index, 1);
    }

    #[test]
    fn test_handle_mouse_scroll_down() {
        let mut app = make_app_with_models(5);
        app.selected_index = 2;

        let event = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse_event(event);
        assert_eq!(app.selected_index, 3);
    }

    // ── handle_resize ───────────────────────────────────────────────

    #[test]
    fn test_handle_resize() {
        let mut app = make_app();
        assert_eq!(app.terminal_width, 80);
        assert_eq!(app.terminal_height, 24);

        app.handle_resize(120, 40);
        assert_eq!(app.terminal_width, 120);
        assert_eq!(app.terminal_height, 40);
        assert_eq!(app.max_visible_items, 20);
    }

    #[test]
    fn test_handle_resize_small_terminal() {
        let mut app = make_app();
        app.handle_resize(40, 12);
        assert_eq!(app.terminal_width, 40);
        assert_eq!(app.terminal_height, 12);
        assert_eq!(app.max_visible_items, 20);
    }

    #[test]
    fn test_handle_resize_preserves_rendered_capacity() {
        let mut app = make_app_with_models(5);
        app.selected_index = 4;
        app.scroll_offset = 2;
        app.max_visible_items = 3;

        app.handle_resize(80, 24);

        assert_eq!(app.max_visible_items, 3);
        assert_eq!(app.selected_index, 4);
        assert_eq!(app.scroll_offset, 2);
    }

    #[test]
    fn test_set_max_visible_items_clamps_scroll_offset() {
        let mut app = make_app_with_models(10);
        app.selected_index = 9;
        app.scroll_offset = 9;

        app.set_max_visible_items(3);

        assert_eq!(app.max_visible_items, 3);
        assert_eq!(app.selected_index, 9);
        assert_eq!(app.scroll_offset, 7);
    }

    // ── on_tick ─────────────────────────────────────────────────────

    #[test]
    fn test_on_tick_increments_frame() {
        let mut app = make_app();
        assert_eq!(app.spinner_frame, 0);

        app.on_tick();
        assert_eq!(app.spinner_frame, 1);

        app.on_tick();
        assert_eq!(app.spinner_frame, 2);
    }

    #[test]
    fn test_on_tick_wraps_spinner_frame() {
        let mut app = make_app();
        app.spinner_frame = 19;
        app.on_tick();
        assert_eq!(app.spinner_frame, 0);
    }

    #[test]
    fn test_on_tick_clears_expired_status() {
        let mut app = make_app();
        app.set_status("test message");
        assert!(app.status_message.is_some());

        app.status_message_time = Some(Instant::now() - Duration::from_secs(5));
        app.auto_refresh = false;

        app.on_tick();
        assert!(app.status_message.is_none());
        assert!(app.status_message_time.is_none());
    }

    #[test]
    fn test_on_tick_keeps_fresh_status() {
        let mut app = make_app();
        app.auto_refresh = false;
        app.set_status("fresh message");

        app.on_tick();
        assert!(app.status_message.is_some());
        assert_eq!(app.status_message.as_ref().unwrap(), "fresh message");
    }

    // ── click area management ───────────────────────────────────────

    #[test]
    fn test_clear_click_areas() {
        let mut app = make_app();
        app.add_click_area(Rect::new(0, 0, 10, 10), ClickAction::Tab(Tab::Models));
        app.add_click_area(Rect::new(10, 0, 10, 10), ClickAction::Tab(Tab::Daily));
        assert_eq!(app.click_areas.len(), 2);

        app.clear_click_areas();
        assert_eq!(app.click_areas.len(), 0);
    }

    // ── narrow detection ────────────────────────────────────────────

    #[test]
    fn test_is_narrow() {
        let mut app = make_app();
        app.terminal_width = 79;
        assert!(app.is_narrow());

        app.terminal_width = 80;
        assert!(!app.is_narrow());
    }

    #[test]
    fn test_is_very_narrow() {
        let mut app = make_app();
        app.terminal_width = 59;
        assert!(app.is_very_narrow());

        app.terminal_width = 60;
        assert!(!app.is_very_narrow());
    }

    // ── HourlyViewMode tests ─────────────────────────────────────────

    #[test]
    fn test_hourly_view_mode_default() {
        let mode = HourlyViewMode::default();
        assert_eq!(mode, HourlyViewMode::Table);
    }

    #[test]
    fn test_hourly_view_mode_toggle() {
        let mut app = make_app();
        assert_eq!(app.hourly_view_mode, HourlyViewMode::Table);

        // Toggle to Profile when on Hourly tab
        app.current_tab = Tab::Hourly;
        app.handle_key_event(key(KeyCode::Char('v')));
        assert_eq!(app.hourly_view_mode, HourlyViewMode::Profile);

        // Toggle back to Table
        app.handle_key_event(key(KeyCode::Char('v')));
        assert_eq!(app.hourly_view_mode, HourlyViewMode::Table);
    }

    #[test]
    fn test_hourly_view_mode_no_toggle_on_other_tabs() {
        let mut app = make_app();
        assert_eq!(app.hourly_view_mode, HourlyViewMode::Table);

        // 'v' should not toggle when not on Hourly tab
        app.current_tab = Tab::Overview;
        app.handle_key_event(key(KeyCode::Char('v')));
        assert_eq!(app.hourly_view_mode, HourlyViewMode::Table);

        app.current_tab = Tab::Daily;
        app.handle_key_event(key(KeyCode::Char('v')));
        assert_eq!(app.hourly_view_mode, HourlyViewMode::Table);
    }

    // ── build_model_shade_map ───────────────────────────────────────

    fn model_usage(name: &str, cost: f64, workspace: Option<&str>) -> ModelUsage {
        ModelUsage {
            model: name.to_string(),
            provider: "anthropic".to_string(),
            client: "claude".to_string(),
            workspace_key: workspace.map(String::from),
            workspace_label: workspace.map(String::from),
            tokens: TokenBreakdown::default(),
            cost,
            performance: Default::default(),
            session_count: 1,
        }
    }

    fn shade_key(provider: &str, model: &str) -> String {
        super::super::colors::model_shade_key(provider, model)
    }

    #[test]
    fn test_shade_map_assigns_rank_0_to_highest_cost() {
        let mut app = make_app();
        app.data.models = vec![
            model_usage("claude-haiku-4-5", 10.0, None),
            model_usage("claude-opus-4-5", 100.0, None),
            model_usage("claude-sonnet-4-5", 50.0, None),
        ];
        app.build_model_shade_map();

        let opus = app
            .model_shade_map
            .get(&shade_key("anthropic", "claude-opus-4-5"))
            .copied()
            .unwrap();
        let sonnet = app
            .model_shade_map
            .get(&shade_key("anthropic", "claude-sonnet-4-5"))
            .copied()
            .unwrap();
        let haiku = app
            .model_shade_map
            .get(&shade_key("anthropic", "claude-haiku-4-5"))
            .copied()
            .unwrap();

        // Rank 0 is the base Anthropic coral; ranks below lighten toward white.
        assert_eq!(opus, get_provider_shade("anthropic", 0));
        assert_eq!(sonnet, get_provider_shade("anthropic", 1));
        assert_eq!(haiku, get_provider_shade("anthropic", 2));
    }

    #[test]
    fn test_shade_map_dedupes_same_model_across_workspaces() {
        // Same model appearing N times in different workspaces (as happens
        // under GroupBy::WorkspaceModel) must not inflate the rank count.
        let mut app = make_app();
        app.data.models = vec![
            model_usage("claude-sonnet-4-5", 20.0, Some("ws-a")),
            model_usage("claude-sonnet-4-5", 20.0, Some("ws-b")),
            model_usage("claude-sonnet-4-5", 20.0, Some("ws-c")),
            model_usage("claude-haiku-4-5", 5.0, None),
        ];
        app.build_model_shade_map();

        // Only two distinct model names should be in the map; sonnet takes
        // rank 0 (aggregate cost 60 > haiku cost 5).
        assert_eq!(app.model_shade_map.len(), 2);
        assert_eq!(
            app.model_shade_map
                .get(&shade_key("anthropic", "claude-sonnet-4-5"))
                .copied(),
            Some(get_provider_shade("anthropic", 0))
        );
        assert_eq!(
            app.model_shade_map
                .get(&shade_key("anthropic", "claude-haiku-4-5"))
                .copied(),
            Some(get_provider_shade("anthropic", 1))
        );
    }

    #[test]
    fn test_shade_map_is_deterministic_on_cost_ties() {
        // All-zero costs (fresh data) must produce a stable shade assignment
        // across refreshes so the chart doesn't flicker.
        let ranks = |app: &App| {
            let a = app
                .model_shade_map
                .get(&shade_key("anthropic", "claude-alpha"))
                .copied();
            let b = app
                .model_shade_map
                .get(&shade_key("anthropic", "claude-beta"))
                .copied();
            let c = app
                .model_shade_map
                .get(&shade_key("anthropic", "claude-gamma"))
                .copied();
            (a, b, c)
        };

        let mut app1 = make_app();
        app1.data.models = vec![
            model_usage("claude-gamma", 0.0, None),
            model_usage("claude-alpha", 0.0, None),
            model_usage("claude-beta", 0.0, None),
        ];
        app1.build_model_shade_map();

        let mut app2 = make_app();
        app2.data.models = vec![
            model_usage("claude-beta", 0.0, None),
            model_usage("claude-gamma", 0.0, None),
            model_usage("claude-alpha", 0.0, None),
        ];
        app2.build_model_shade_map();

        assert_eq!(ranks(&app1), ranks(&app2));
        // alpha sorts first by name so it gets rank 0 on ties.
        assert_eq!(
            app1.model_shade_map
                .get(&shade_key("anthropic", "claude-alpha"))
                .copied(),
            Some(get_provider_shade("anthropic", 0))
        );
    }

    #[test]
    fn test_shade_map_handles_nan_cost() {
        // NaN costs must not propagate into total_cmp ordering surprises or
        // crash the builder.
        let mut app = make_app();
        app.data.models = vec![
            model_usage("claude-nan", f64::NAN, None),
            model_usage("claude-normal", 1.0, None),
        ];
        app.build_model_shade_map();

        assert_eq!(app.model_shade_map.len(), 2);
        // Normal model outranks NaN (which is coerced to 0).
        assert_eq!(
            app.model_shade_map
                .get(&shade_key("anthropic", "claude-normal"))
                .copied(),
            Some(get_provider_shade("anthropic", 0))
        );
    }

    #[test]
    fn test_shade_map_separates_providers() {
        let mut app = make_app();
        app.data.models = vec![
            ModelUsage {
                model: "claude-opus-4-5".to_string(),
                provider: "anthropic".to_string(),
                client: "claude".to_string(),
                workspace_key: None,
                workspace_label: None,
                tokens: TokenBreakdown::default(),
                cost: 10.0,
                performance: Default::default(),
                session_count: 1,
            },
            ModelUsage {
                model: "gpt-5".to_string(),
                provider: "openai".to_string(),
                client: "codex".to_string(),
                workspace_key: None,
                workspace_label: None,
                tokens: TokenBreakdown::default(),
                cost: 1.0,
                performance: Default::default(),
                session_count: 1,
            },
        ];
        app.build_model_shade_map();

        // Each provider ranks independently — both get rank-0 shades.
        assert_eq!(
            app.model_shade_map
                .get(&shade_key("anthropic", "claude-opus-4-5"))
                .copied(),
            Some(get_provider_shade("anthropic", 0))
        );
        assert_eq!(
            app.model_shade_map
                .get(&shade_key("openai", "gpt-5"))
                .copied(),
            Some(get_provider_shade("openai", 0))
        );
    }

    #[test]
    fn test_shade_map_rebuilds_on_update_data() {
        let mut app = make_app();
        app.data.models = vec![model_usage("claude-opus-4-5", 10.0, None)];
        app.build_model_shade_map();
        assert!(app
            .model_shade_map
            .contains_key(&shade_key("anthropic", "claude-opus-4-5")));

        let fresh = UsageData {
            models: vec![model_usage("claude-sonnet-4-5", 5.0, None)],
            ..UsageData::default()
        };
        app.update_data(fresh);

        assert!(!app
            .model_shade_map
            .contains_key(&shade_key("anthropic", "claude-opus-4-5")));
        assert!(app
            .model_shade_map
            .contains_key(&shade_key("anthropic", "claude-sonnet-4-5")));
    }

    #[test]
    fn test_same_model_name_keeps_distinct_provider_colors() {
        let mut app = make_app();
        app.data.models = vec![
            ModelUsage {
                model: "sonnet-shared".to_string(),
                provider: "anthropic".to_string(),
                client: "claude".to_string(),
                workspace_key: None,
                workspace_label: None,
                tokens: TokenBreakdown::default(),
                cost: 10.0,
                performance: Default::default(),
                session_count: 1,
            },
            ModelUsage {
                model: "sonnet-shared".to_string(),
                provider: "openai".to_string(),
                client: "codex".to_string(),
                workspace_key: None,
                workspace_label: None,
                tokens: TokenBreakdown::default(),
                cost: 5.0,
                performance: Default::default(),
                session_count: 1,
            },
        ];
        app.build_model_shade_map();

        assert_eq!(
            app.model_color_for("anthropic", "sonnet-shared"),
            app.theme.color(get_provider_shade("anthropic", 0))
        );
        assert_eq!(
            app.model_color_for("openai", "sonnet-shared"),
            app.theme.color(get_provider_shade("openai", 0))
        );
        assert_ne!(
            app.model_color_for("anthropic", "sonnet-shared"),
            app.model_color_for("openai", "sonnet-shared")
        );
    }
}
