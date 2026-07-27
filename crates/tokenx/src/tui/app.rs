use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Index;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use tokenx_engine::{
    pricing::PricingStatus, ClientId, ClientSelection, ClientUniverse, Generation, UsageQuery,
};

use ratatui::style::Color;

use super::data::{
    AgentEntry, DailyUsage, HourlyUsage, OverviewSummary, PeriodKind, PeriodUsage, UsageModelEntry,
    UsageProjection,
};
use super::generation_controller::{RefreshControl, RefreshRequest, RefreshStatus};
use super::interaction::{
    InteractionOutcome, ListInteraction, MoveCommand, TextViewport, WrapMode,
};
pub use super::local_usage::PeriodDetailSelection;
use super::local_usage::{
    DetailRow, DetailSelections, InstalledGeneration, LocalUsageState, LocalUsageStatus,
    PreparedProjection,
};
use super::model_family::ModelFamily;
use super::session_data::SessionSnapshot;
use super::themes::{Theme, ThemeName};
use super::ui::dialog::{ClientPickerDialog, DialogResult, DialogStack, UiCommand};
use crate::product_paths::ProductPaths;
use crate::settings::Settings;
use crate::subscription::{
    FetchRequest, ProviderId, SubscriptionBatch, SubscriptionInstall, SubscriptionOutput,
    SubscriptionPoll, SubscriptionState,
};

/// Configuration for TUI initialization
pub struct TuiConfig {
    pub theme: Option<ThemeName>,
    pub refresh: u64,
    pub no_refresh: bool,
    pub client_universe: ClientUniverse,
    pub initial_tab: Option<Tab>,
    pub effective_date: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiExit {
    Quit,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyEventOutcome {
    Continue,
    Exit(TuiExit),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Tab {
    Overview,
    Subscription,
    Models,
    Monthly,
    Weekly,
    Daily,
    Hourly,
    Stats,
    Agents,
    Sessions,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[
            Tab::Overview,
            Tab::Subscription,
            Tab::Models,
            Tab::Monthly,
            Tab::Weekly,
            Tab::Daily,
            Tab::Hourly,
            Tab::Stats,
            Tab::Agents,
            Tab::Sessions,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        self.labels().0
    }

    pub fn short_name(&self) -> &'static str {
        self.labels().1
    }

    fn labels(self) -> (&'static str, &'static str) {
        match self {
            Tab::Overview => ("Overview", "Ovw"),
            Tab::Subscription => ("Subscription", "Sub"),
            Tab::Models => ("Models", "Mod"),
            Tab::Monthly => ("Monthly", "Mth"),
            Tab::Weekly => ("Weekly", "Wk"),
            Tab::Daily => ("Daily", "Day"),
            Tab::Hourly => ("Hourly", "Hr"),
            Tab::Stats => ("Stats", "Sta"),
            Tab::Agents => ("Agents", "Agt"),
            Tab::Sessions => ("Sessions", "Ses"),
        }
    }

    /// Whether this tab projects the installed local generation.
    /// Subscription has its own remote fetch lifecycle and must remain
    /// usable while local input acquisition is cold-loading or has failed.
    pub(crate) fn depends_on_local_generation(self) -> bool {
        self != Tab::Subscription
    }

    pub fn next(self) -> Tab {
        match self {
            Tab::Overview => Tab::Subscription,
            Tab::Subscription => Tab::Models,
            Tab::Models => Tab::Monthly,
            Tab::Monthly => Tab::Weekly,
            Tab::Weekly => Tab::Daily,
            Tab::Daily => Tab::Hourly,
            Tab::Hourly => Tab::Stats,
            Tab::Stats => Tab::Agents,
            Tab::Agents => Tab::Sessions,
            Tab::Sessions => Tab::Overview,
        }
    }

    pub fn prev(self) -> Tab {
        match self {
            Tab::Overview => Tab::Sessions,
            Tab::Subscription => Tab::Overview,
            Tab::Models => Tab::Subscription,
            Tab::Monthly => Tab::Models,
            Tab::Weekly => Tab::Monthly,
            Tab::Daily => Tab::Weekly,
            Tab::Hourly => Tab::Daily,
            Tab::Stats => Tab::Hourly,
            Tab::Agents => Tab::Stats,
            Tab::Sessions => Tab::Agents,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StatusMessageKind {
    #[default]
    General,
    Generation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StatusTone {
    #[default]
    Info,
    Success,
    Warning,
    Danger,
}

fn pricing_warning(status: PricingStatus) -> Option<&'static str> {
    match status {
        PricingStatus::Available => None,
        PricingStatus::CachedFallback => Some("Pricing refresh failed; using cached rates"),
        PricingStatus::Unavailable => Some("Pricing unavailable; costs may be missing"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelOrderKey {
    usage_revision: u64,
    sort_field: SortField,
    sort_direction: SortDirection,
    detail: Option<ModelDetailSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsageOrderKey {
    usage_revision: u64,
    sort_field: SortField,
    sort_direction: SortDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailOrderSelection {
    Daily(NaiveDate),
    Period(PeriodDetailSelection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DetailOrderKey {
    usage_revision: u64,
    selection: DetailOrderSelection,
    sort_field: SortField,
    sort_direction: SortDirection,
}

#[derive(Debug)]
struct CachedRenderOrder<K> {
    key: K,
    order: Arc<[usize]>,
}

#[derive(Debug, Default)]
struct RenderOrderCache {
    models: Option<CachedRenderOrder<ModelOrderKey>>,
    daily: Option<CachedRenderOrder<UsageOrderKey>>,
    hourly: Option<CachedRenderOrder<UsageOrderKey>>,
    detail: Option<CachedRenderOrder<DetailOrderKey>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DetailSortContextKind {
    Models,
    Daily,
    Period,
}

#[derive(Debug, Clone, Copy, Default)]
struct DetailSortContext {
    list_sort_before_detail: Option<(SortField, SortDirection)>,
    detail_sort_state: Option<(SortField, SortDirection)>,
}

pub struct ClickArea {
    pub rect: Rect,
    pub action: ClickAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDetailSelection {
    pub model: String,
    pub client: Option<ClientId>,
}

enum ModelDetailClientUpdate {
    Inactive,
    Ready(Vec<UsageModelEntry>),
    MissingSelection,
}

#[derive(Debug, Clone)]
pub enum ClickAction {
    Tab(Tab),
    Sort(SortField),
    GraphCell { week: usize, day: usize },
}

fn client_ids_text(clients: &[ClientId]) -> String {
    clients
        .iter()
        .map(|client| client.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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

fn sort_detail_order(
    order: &mut [usize],
    rows: &[DetailRow],
    field: SortField,
    direction: SortDirection,
) {
    let tie_breaker = |a: &DetailRow, b: &DetailRow| {
        a.clients
            .cmp(&b.clients)
            .then_with(|| a.model.cmp(&b.model))
            .then_with(|| a.provider.cmp(&b.provider))
    };

    match (field, direction) {
        (SortField::Cost, SortDirection::Descending) => order.sort_by(|a, b| {
            rows[*b]
                .cost
                .total_cmp(&rows[*a].cost)
                .then_with(|| tie_breaker(&rows[*a], &rows[*b]))
        }),
        (SortField::Cost, SortDirection::Ascending) => order.sort_by(|a, b| {
            rows[*a]
                .cost
                .total_cmp(&rows[*b].cost)
                .then_with(|| tie_breaker(&rows[*a], &rows[*b]))
        }),
        (SortField::Tokens, SortDirection::Descending) => order.sort_by(|a, b| {
            rows[*b]
                .tokens
                .total()
                .cmp(&rows[*a].tokens.total())
                .then_with(|| tie_breaker(&rows[*a], &rows[*b]))
        }),
        (SortField::Tokens, SortDirection::Ascending) => order.sort_by(|a, b| {
            rows[*a]
                .tokens
                .total()
                .cmp(&rows[*b].tokens.total())
                .then_with(|| tie_breaker(&rows[*a], &rows[*b]))
        }),
        (SortField::Date, _) => {
            order.sort_by(|a, b| tie_breaker(&rows[*a], &rows[*b]));
        }
    }
}

pub(crate) struct OrderedDetailRows<'a> {
    rows: &'a [DetailRow],
    order: Arc<[usize]>,
}

impl<'a> OrderedDetailRows<'a> {
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub(crate) fn get(&self, index: usize) -> Option<&'a DetailRow> {
        self.rows.get(*self.order.get(index)?)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &'a DetailRow> + '_ {
        self.order.iter().map(|index| &self.rows[*index])
    }
}

impl Index<usize> for OrderedDetailRows<'_> {
    type Output = DetailRow;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index)
            .expect("ordered detail row index is out of bounds")
    }
}

pub struct App {
    pub current_tab: Tab,
    pub theme: Theme,
    pub settings: Settings,
    product_paths: ProductPaths,
    local_usage: LocalUsageState,

    pub sort_field: SortField,
    pub sort_direction: SortDirection,
    tab_sort_state: HashMap<Tab, (SortField, SortDirection)>,
    pub chart_granularity: ChartGranularity,

    pub scroll_offset: usize,
    pub selected_index: usize,
    pub max_visible_items: usize,
    list_interactions: HashMap<Tab, ListInteraction>,
    pub(crate) subscription_viewport: TextViewport,
    subscription_text_total_lines: usize,
    pub(crate) hourly_profile_viewport: TextViewport,
    hourly_profile_text_total_lines: usize,
    pub selected_daily_detail_date: Option<NaiveDate>,
    pub selected_period_detail: Option<PeriodDetailSelection>,
    pub selected_model_detail: Option<ModelDetailSelection>,
    model_detail_models: Option<Vec<UsageModelEntry>>,
    detail_sort_contexts: HashMap<DetailSortContextKind, DetailSortContext>,
    usage_revision: u64,
    render_order_cache: RefCell<RenderOrderCache>,

    pub selected_graph_cell: Option<(usize, usize)>,
    stats_auto_select_today_pending: bool,

    refresh_status: RefreshStatus,
    refresh_requests: VecDeque<RefreshRequest>,
    refresh_controls: VecDeque<RefreshControl>,

    pub status_message: Option<String>,
    pub status_message_time: Option<Instant>,
    status_message_kind: StatusMessageKind,
    status_message_tone: StatusTone,
    generation_cache_warning: Option<String>,
    pricing_status: PricingStatus,
    pub subscription_status_message: Option<String>,
    pub subscription_status_message_time: Option<Instant>,
    subscription_status_message_tone: StatusTone,

    pub terminal_width: u16,
    pub terminal_height: u16,

    pub click_areas: Vec<ClickArea>,

    pub spinner_frame: usize,

    /// Monotonic tick counter driving the Overview fun-fact ticker (spinner
    /// frames wrap too fast to scroll a long string).
    pub(crate) ticker_tick: u32,

    pub dialog_stack: DialogStack,

    pub hourly_view_mode: HourlyViewMode,

    subscription: SubscriptionState,
}

impl App {
    #[cfg(test)]
    fn test_product_paths() -> ProductPaths {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
        ProductPaths::at(std::env::temp_dir().join(format!(
            "tokenx-app-test-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        )))
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(config: TuiConfig) -> Result<Self> {
        Self::new_for_test_with_settings(config, Settings::default())
    }

    #[cfg(test)]
    pub(crate) fn new_for_test_with_settings(
        config: TuiConfig,
        settings: Settings,
    ) -> Result<Self> {
        Self::new(config, settings, Self::test_product_paths())
    }

    #[cfg(test)]
    pub(crate) fn new_for_test_with_settings_and_paths(
        config: TuiConfig,
        settings: Settings,
        product_paths: ProductPaths,
    ) -> Result<Self> {
        Self::new(config, settings, product_paths)
    }

    pub(crate) fn new(
        config: TuiConfig,
        settings: Settings,
        product_paths: ProductPaths,
    ) -> Result<Self> {
        let theme_name = config.theme.unwrap_or(settings.color_palette);
        let theme = Theme::from_name(theme_name);

        let client_universe = config.client_universe.clone();
        let effective_date = config.effective_date;

        let auto_refresh_interval = if config.refresh > 0 {
            Duration::from_secs(config.refresh)
        } else {
            settings.configured_auto_refresh_interval()
        };

        let auto_refresh = if config.no_refresh {
            false
        } else {
            config.refresh > 0 || settings.auto_refresh_enabled
        };
        let subscription_enabled = settings.subscription.enabled;
        let local_usage = LocalUsageState::new(UsageQuery::full(
            &client_universe,
            tokenx_engine::GroupBy::default(),
            effective_date,
        ));
        let dialog_stack = DialogStack::new(theme.clone());
        let requested_tab = config.initial_tab.unwrap_or(Tab::Overview);
        if !Self::tab_visible(&settings, requested_tab) {
            anyhow::bail!(
                "TUI tab `{}` is disabled in settings.json",
                requested_tab.as_str().to_ascii_lowercase()
            );
        }
        let current_tab = requested_tab;
        let (sort_field, sort_direction) = Self::default_sort_for_tab(current_tab);
        let subscription = if subscription_enabled {
            SubscriptionState::new(
                settings.subscription.providers.clone(),
                crate::subscription::cache::load(&product_paths.subscription_cache_file()),
            )
        } else {
            SubscriptionState::disabled()
        };

        let mut app = Self {
            current_tab,
            theme,
            settings,
            product_paths,
            local_usage,
            sort_field,
            sort_direction,
            tab_sort_state: HashMap::new(),
            chart_granularity: ChartGranularity::default(),
            scroll_offset: 0,
            selected_index: 0,
            max_visible_items: 20,
            list_interactions: HashMap::new(),
            subscription_viewport: TextViewport::default(),
            subscription_text_total_lines: 0,
            hourly_profile_viewport: TextViewport::default(),
            hourly_profile_text_total_lines: 0,
            selected_daily_detail_date: None,
            selected_period_detail: None,
            selected_model_detail: None,
            model_detail_models: None,
            detail_sort_contexts: HashMap::new(),
            usage_revision: 0,
            render_order_cache: RefCell::new(RenderOrderCache::default()),
            selected_graph_cell: None,
            stats_auto_select_today_pending: current_tab == Tab::Stats,
            refresh_status: RefreshStatus::new(auto_refresh, auto_refresh_interval, Duration::ZERO),
            refresh_requests: VecDeque::new(),
            refresh_controls: VecDeque::new(),
            status_message: None,
            status_message_time: None,
            status_message_kind: StatusMessageKind::General,
            status_message_tone: StatusTone::Info,
            generation_cache_warning: None,
            pricing_status: PricingStatus::Available,
            subscription_status_message: None,
            subscription_status_message_time: None,
            subscription_status_message_tone: StatusTone::Info,
            terminal_width: 80,
            terminal_height: 24,
            click_areas: Vec::new(),
            spinner_frame: 0,
            ticker_tick: 0,
            dialog_stack,
            hourly_view_mode: HourlyViewMode::default(),
            subscription,
        };
        app.try_auto_select_stats_today();
        app.maybe_fetch_subscription_on_entry();
        Ok(app)
    }

    pub(crate) fn is_background_loading(&self) -> bool {
        self.refresh_status.loading()
    }

    pub(crate) fn background_load_elapsed(&self) -> Option<Duration> {
        self.refresh_status.loading_elapsed()
    }

    pub fn has_installed_generation(&self) -> bool {
        self.installed_generation().is_some()
    }

    pub(crate) fn installed_generation(&self) -> Option<&InstalledGeneration> {
        self.local_usage.installed()
    }

    pub(crate) fn generation_health(&self) -> Option<&tokenx_engine::input_health::HealthSummary> {
        self.installed_generation()
            .map(InstalledGeneration::generation)
            .map(Generation::health)
    }

    pub(crate) fn local_usage_status(&self) -> LocalUsageStatus<'_> {
        self.local_usage.status()
    }

    pub(crate) fn is_cold_loading(&self) -> bool {
        self.is_background_loading() && !self.has_installed_generation()
    }

    pub(crate) fn is_cold_failed(&self) -> bool {
        matches!(self.local_usage.status(), LocalUsageStatus::Failed { .. })
    }

    pub(crate) fn overview_summary(&self) -> &OverviewSummary {
        self.require_installed_generation().overview()
    }

    pub(crate) fn usage(&self) -> &UsageProjection {
        self.require_installed_generation().view()
    }

    pub(crate) fn period_usage(&self, kind: PeriodKind) -> &[PeriodUsage] {
        self.require_installed_generation().periods(kind)
    }

    fn detail_selections(&self) -> DetailSelections {
        DetailSelections {
            daily: self.selected_daily_detail_date,
            period: self.selected_period_detail,
        }
    }

    pub(crate) fn session_snapshot(&self) -> &SessionSnapshot {
        self.require_installed_generation().sessions()
    }

    pub(crate) fn total_input_bytes(&self) -> u64 {
        self.require_installed_generation()
            .generation()
            .input_footprint()
            .total_bytes()
            .expect("validated input footprint must fit in u64")
    }

    fn require_installed_generation(&self) -> &InstalledGeneration {
        self.installed_generation()
            .expect("local projection data requires an installed generation")
    }

    pub(crate) fn group_by(&self) -> tokenx_engine::GroupBy {
        self.local_usage.query().group_by
    }

    pub(crate) fn selected_clients(&self) -> impl ExactSizeIterator<Item = ClientId> + '_ {
        self.local_usage.query().clients.iter()
    }

    pub(crate) fn client_universe(&self) -> ClientUniverse {
        self.installed_generation()
            .map(InstalledGeneration::generation)
            .map(Generation::universe)
            .cloned()
            .unwrap_or_else(|| {
                ClientUniverse::new(self.local_usage.query().clients.iter())
                    .expect("initial TUI projection has a non-empty client universe")
            })
    }

    pub(crate) fn effective_date(&self) -> NaiveDate {
        self.local_usage.query().effective_date
    }

    pub(crate) fn current_calendar_hour(&self) -> chrono::NaiveDateTime {
        self.calendar_context().current_hour()
    }

    pub(crate) fn calendar_context(&self) -> tokenx_engine::CalendarContext {
        *self
            .require_installed_generation()
            .generation()
            .acquisition_config()
            .calendar()
    }

    pub fn has_enabled_subscription_providers(&self) -> bool {
        !self.subscription.enabled().is_empty()
    }

    pub(crate) fn subscription_outputs(&self) -> &[SubscriptionOutput] {
        self.subscription.outputs()
    }

    pub(crate) fn subscription_errors(&self) -> &[crate::subscription::SubscriptionError] {
        self.subscription.errors()
    }

    pub(crate) fn has_subscription_fetch_history(&self) -> bool {
        self.subscription.has_fetch_history()
    }

    pub(crate) fn last_subscription_check(&self) -> Option<Instant> {
        self.subscription.last_checked()
    }

    pub(crate) fn refresh_status(&self) -> RefreshStatus {
        self.refresh_status
    }

    pub(crate) fn auto_refresh_enabled(&self) -> bool {
        self.refresh_status.automatic()
    }

    pub(crate) fn auto_refresh_interval(&self) -> Duration {
        self.refresh_status.interval()
    }

    pub(crate) fn last_refresh_elapsed(&self) -> Duration {
        self.refresh_status.elapsed()
    }

    pub(crate) fn set_refresh_status(&mut self, status: RefreshStatus) {
        self.refresh_status = status;
    }

    #[cfg(test)]
    pub(crate) fn set_refresh_status_for_test(
        &mut self,
        automatic: bool,
        interval: Duration,
        last_checked: Instant,
    ) {
        self.refresh_status = RefreshStatus::new(automatic, interval, last_checked.elapsed());
    }

    #[cfg(test)]
    pub(crate) fn set_refresh_loading_for_test(&mut self, loading: bool) {
        self.refresh_status.set_loading_for_test(loading);
    }

    pub(crate) fn take_refresh_requests(&mut self) -> Vec<RefreshRequest> {
        self.refresh_requests.drain(..).collect()
    }

    pub(crate) fn take_refresh_controls(&mut self) -> Vec<RefreshControl> {
        self.refresh_controls.drain(..).collect()
    }

    pub(crate) fn persist_refresh_policy(
        &mut self,
        automatic: bool,
        interval: Duration,
        message: String,
    ) {
        self.settings.auto_refresh_enabled = automatic;
        self.settings.auto_refresh_ms = interval.as_millis() as u64;
        match self.settings.save(&self.product_paths) {
            Ok(()) => self.set_status(&message),
            Err(error) => self.set_status_with_tone(
                &format!("{message} (save failed: {error})"),
                StatusTone::Danger,
            ),
        }
    }

    fn handle_dialog_result(&mut self, result: DialogResult) {
        if let DialogResult::Submit(command) = result {
            self.apply_ui_command(command);
        }
    }

    fn apply_ui_command(&mut self, command: UiCommand) {
        let (selected_clients, group_by) = match command {
            UiCommand::ProjectClients(clients) => (clients, self.group_by()),
            UiCommand::ProjectGroupBy(group_by) => {
                (self.selected_clients().collect::<HashSet<_>>(), group_by)
            }
        };
        self.apply_projection(selected_clients, group_by);
    }

    fn apply_projection(
        &mut self,
        selected_clients: HashSet<ClientId>,
        group_by: tokenx_engine::GroupBy,
    ) {
        let current_clients = self.selected_clients().collect::<HashSet<_>>();
        let client_changed = selected_clients != current_clients;
        let group_changed = group_by != self.group_by();
        if !client_changed && !group_changed {
            return;
        }
        if selected_clients.is_empty()
            || selected_clients
                .iter()
                .any(|client| !self.client_universe().contains(*client))
        {
            self.set_status_with_tone(
                "Client selection is outside the loaded client universe",
                StatusTone::Danger,
            );
            return;
        }
        if !self.has_installed_generation() {
            self.set_status_with_tone("Local data is not loaded yet", StatusTone::Warning);
            return;
        }

        let query = UsageQuery {
            clients: match ClientSelection::new(selected_clients.iter().copied()) {
                Ok(clients) => clients,
                Err(error) => {
                    self.set_status_with_tone(
                        &format!("Client projection failed: {error:#}"),
                        StatusTone::Danger,
                    );
                    return;
                }
            },
            group_by,
            effective_date: self.effective_date(),
        };
        let projection = match self
            .local_usage
            .prepare_projection(query, self.detail_selections())
        {
            Ok(projection) => projection,
            Err(error) => {
                let operation = if client_changed && !group_changed {
                    "Client projection"
                } else {
                    "Group By projection"
                };
                let diagnostic = format!("{operation} failed: {error:#}");
                self.set_status_with_tone(&diagnostic, StatusTone::Danger);
                return;
            }
        };
        let detail_update = if client_changed && !group_changed {
            match self.model_detail_update_for_clients(projection.view(), &selected_clients) {
                Ok(update) => update,
                Err(error) => {
                    self.set_status_with_tone(
                        &format!(
                            "Client projection failed while refreshing model details: {error:#}"
                        ),
                        StatusTone::Danger,
                    );
                    return;
                }
            }
        } else {
            ModelDetailClientUpdate::Inactive
        };

        let client_status = if group_changed {
            self.clear_model_detail_state(client_changed);
            None
        } else if client_changed {
            Some(match detail_update {
                ModelDetailClientUpdate::Ready(models) => {
                    self.model_detail_models = Some(models);
                    (
                        "Clients filtered locally; model details updated",
                        StatusTone::Success,
                    )
                }
                ModelDetailClientUpdate::MissingSelection => {
                    self.clear_model_detail_state(true);
                    (
                        "Selected model is not available for the current Client filter",
                        StatusTone::Warning,
                    )
                }
                ModelDetailClientUpdate::Inactive => {
                    self.clear_model_detail_state(true);
                    ("Clients filtered locally", StatusTone::Success)
                }
            })
        } else {
            None
        };
        self.install_projected_data(projection);
        if let Some((status, tone)) = client_status {
            self.set_generation_status_with_tone(status, tone);
        } else {
            self.set_generation_status_with_tone(
                &format!("Regrouped by {group_by}"),
                StatusTone::Success,
            );
        }
    }

    fn install_projected_data(&mut self, projection: PreparedProjection) {
        self.replace_usage_projection(projection, false);
        crate::acquisition::trim_allocator();
    }

    fn graph_cell_for_date(&self, date: NaiveDate) -> Option<(usize, usize)> {
        self.installed_generation()?
            .view()
            .graph
            .weeks
            .iter()
            .enumerate()
            .find_map(|(week_idx, week)| {
                week.iter()
                    .position(|day| day.as_ref().is_some_and(|day| day.date == date))
                    .map(|day_idx| (week_idx, day_idx))
            })
    }

    fn graph_date_for_cell(&self, (week_idx, day_idx): (usize, usize)) -> Option<NaiveDate> {
        self.installed_generation()?
            .view()
            .graph
            .weeks
            .get(week_idx)?
            .get(day_idx)?
            .as_ref()
            .map(|day| day.date)
    }

    fn request_stats_today_selection(&mut self) {
        self.selected_graph_cell = None;
        self.stats_auto_select_today_pending = true;
        self.try_auto_select_stats_today();
    }

    fn try_auto_select_stats_today(&mut self) {
        if !self.stats_auto_select_today_pending || self.current_tab != Tab::Stats {
            return;
        }

        if let Some(cell) = self.graph_cell_for_date(self.effective_date()) {
            self.selected_graph_cell = Some(cell);
            self.stats_auto_select_today_pending = false;
        }
    }

    #[cfg(test)]
    pub fn update_data(&mut self, data: UsageProjection) {
        self.ensure_test_generation();
        self.replace_usage_data_for_test(data, true);
        crate::acquisition::trim_allocator();
    }

    #[cfg(test)]
    fn update_projected_data(&mut self, data: UsageProjection) {
        self.ensure_test_generation();
        self.replace_usage_data_for_test(data, false);
        crate::acquisition::trim_allocator();
    }

    fn capture_usage_selection(&mut self, mark_refresh: bool) -> (bool, Option<NaiveDate>) {
        if mark_refresh {
            self.clear_model_detail_state(true);
        }
        let had_graph_selection = self.selected_graph_cell.is_some();
        let selected_graph_date = self
            .selected_graph_cell
            .and_then(|cell| self.graph_date_for_cell(cell));
        (had_graph_selection, selected_graph_date)
    }

    fn reconcile_usage_selection(
        &mut self,
        had_graph_selection: bool,
        selected_graph_date: Option<NaiveDate>,
    ) {
        if had_graph_selection {
            self.selected_graph_cell =
                selected_graph_date.and_then(|date| self.graph_cell_for_date(date));
        }
        self.try_auto_select_stats_today();

        // Exit Daily-detail mode if the refresh dropped the day we were
        // viewing; otherwise `daily_detail_rows()` would return
        // empty while the user is still nominally in detail mode.
        if let Some(date) = self.selected_daily_detail_date {
            if !self.usage().daily.iter().any(|day| day.date == date) {
                self.leave_daily_detail_sort_context();
                self.selected_daily_detail_date = None;
                self.set_current_list_interaction(self.stored_list_interaction(Tab::Daily));
            }
        }
        if let Some(selection) = self.selected_period_detail {
            let period_still_exists = self.period_usage(selection.kind).iter().any(|period| {
                period.start_date == selection.start_date && period.end_date == selection.end_date
            });
            if !period_still_exists {
                let tab = Self::period_tab(selection.kind);
                self.leave_period_detail_sort_context();
                self.selected_period_detail = None;
                self.set_current_list_interaction(self.stored_list_interaction(tab));
            }
        }

        self.clamp_selection();
    }

    fn replace_usage_projection(&mut self, projection: PreparedProjection, mark_refresh: bool) {
        let (had_graph_selection, selected_graph_date) = self.capture_usage_selection(mark_refresh);
        self.local_usage.install_projection(projection);
        self.bump_usage_revision();
        self.reconcile_usage_selection(had_graph_selection, selected_graph_date);
    }

    #[cfg(test)]
    fn replace_usage_data_for_test(&mut self, data: UsageProjection, mark_refresh: bool) {
        let (had_graph_selection, selected_graph_date) = self.capture_usage_selection(mark_refresh);
        self.local_usage
            .replace_view_for_test(data, self.detail_selections());
        self.bump_usage_revision();
        self.reconcile_usage_selection(had_graph_selection, selected_graph_date);
    }

    fn bump_usage_revision(&mut self) {
        self.usage_revision = self.usage_revision.wrapping_add(1);
        *self.render_order_cache.get_mut() = RenderOrderCache::default();
    }

    fn project_generation(
        &self,
        group_by: tokenx_engine::GroupBy,
        selected_clients: &HashSet<ClientId>,
    ) -> Result<UsageProjection> {
        let clients = ClientSelection::new(selected_clients.iter().copied())?;
        self.local_usage.project_view(&UsageQuery {
            clients,
            group_by,
            effective_date: self.effective_date(),
        })
    }

    pub(crate) fn install_generation(&mut self, generation: Generation) -> Result<()> {
        if generation.universe() != &self.client_universe() {
            anyhow::bail!("generation client universe does not match TUI acquisition universe");
        }

        let pricing_status = generation.pricing_status();
        let (had_graph_selection, selected_graph_date) = self.capture_usage_selection(true);
        self.local_usage
            .install_generation(generation, self.detail_selections())?;
        self.bump_usage_revision();
        self.pricing_status = pricing_status;
        self.reconcile_usage_selection(had_graph_selection, selected_graph_date);
        crate::acquisition::trim_allocator();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn install_generation_fixture(
        &mut self,
        usage_index: tokenx_engine::FrozenUsageIndex,
        sessions: Vec<tokenx_engine::SessionUsage>,
        input_footprint: tokenx_engine::InputFootprint,
    ) {
        self.install_generation_fixture_with_health(
            usage_index,
            sessions,
            input_footprint,
            tokenx_engine::input_health::HealthSummary::default(),
        );
    }

    #[cfg(test)]
    pub(crate) fn install_generation_fixture_with_health(
        &mut self,
        usage_index: tokenx_engine::FrozenUsageIndex,
        sessions: Vec<tokenx_engine::SessionUsage>,
        input_footprint: tokenx_engine::InputFootprint,
        health: tokenx_engine::input_health::HealthSummary,
    ) {
        let generation = super::generation_fixture_with_health(
            self.client_universe().iter(),
            usage_index,
            sessions,
            input_footprint,
            health,
        );
        self.install_generation(generation)
            .expect("test generation installs");
    }

    #[cfg(test)]
    pub(crate) fn install_generation_fixture_with_pricing_diagnostics(
        &mut self,
        diagnostics: tokenx_engine::pricing::PricingDiagnostics,
    ) {
        let generation = super::generation_fixture_with_health_and_pricing(
            self.client_universe().iter(),
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            tokenx_engine::InputFootprint::default(),
            tokenx_engine::input_health::HealthSummary::default(),
            diagnostics,
        );
        self.install_generation(generation)
            .expect("test generation installs");
    }

    #[cfg(test)]
    fn ensure_test_generation(&mut self) {
        if !self.has_installed_generation() {
            self.install_generation_fixture(
                tokenx_engine::FrozenUsageIndex::new(),
                Vec::new(),
                tokenx_engine::InputFootprint::default(),
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn usage_mut_for_test(&mut self) -> super::local_usage::UsageProjectionMut<'_> {
        self.ensure_test_generation();
        self.bump_usage_revision();
        self.local_usage.view_mut()
    }

    #[cfg(test)]
    pub(crate) fn replace_session_snapshot_for_test(&mut self, snapshot: SessionSnapshot) {
        self.ensure_test_generation();
        self.local_usage.replace_sessions_for_test(snapshot);
        self.bump_usage_revision();
    }

    #[cfg(test)]
    pub(crate) fn set_selected_clients_for_test(&mut self, clients: HashSet<ClientId>) {
        let query = UsageQuery {
            clients: ClientSelection::new(clients).expect("test client selection is non-empty"),
            group_by: self.group_by(),
            effective_date: self.effective_date(),
        };
        self.local_usage.set_query_for_test(query);
        self.bump_usage_revision();
    }

    #[cfg(test)]
    pub(crate) fn set_group_by_for_test(&mut self, group_by: tokenx_engine::GroupBy) {
        let query = UsageQuery {
            clients: self.local_usage.query().clients.clone(),
            group_by,
            effective_date: self.effective_date(),
        };
        self.local_usage.set_query_for_test(query);
        self.bump_usage_revision();
    }

    #[cfg(test)]
    pub(crate) fn generation_for_test(&self) -> Option<&Generation> {
        self.installed_generation()
            .map(InstalledGeneration::generation)
    }

    pub(crate) fn fail_local_usage_load(&mut self, diagnostic: String) {
        self.local_usage.fail_acquisition(diagnostic);
    }

    #[cfg(test)]
    pub(crate) fn fail_refresh_for_test(&mut self, diagnostic: impl Into<String>) {
        self.set_refresh_loading_for_test(false);
        self.fail_local_usage_load(diagnostic.into());
    }

    pub fn model_color(&self, model_id: &str) -> Color {
        self.theme
            .model_identity_color(ModelFamily::from_model_id(model_id))
    }

    pub(crate) fn family_color(&self, family: ModelFamily) -> Color {
        self.theme.model_identity_color(family)
    }

    pub fn client_color(&self, client: ClientId) -> Color {
        self.theme.client_identity_color(Some(client))
    }

    pub(crate) fn set_generation_cache_warning(&mut self, warning: Option<String>) {
        self.generation_cache_warning = warning;
    }

    pub(crate) fn generation_cache_warning(&self) -> Option<&str> {
        self.generation_cache_warning.as_deref()
    }

    pub(crate) fn pricing_warning(&self) -> Option<&'static str> {
        pricing_warning(self.pricing_status)
    }

    pub fn on_tick(&mut self) {
        self.on_tick_for_date(self.effective_date());
    }

    pub(super) fn on_tick_for_date(&mut self, current_date: NaiveDate) {
        self.spinner_frame = (self.spinner_frame + 1) % 20;
        self.ticker_tick = self.ticker_tick.wrapping_add(1);
        self.advance_effective_date(current_date);

        if let Some(status_time) = self.status_message_time {
            if status_time.elapsed() > Duration::from_secs(3) {
                self.status_message = None;
                self.status_message_time = None;
                self.status_message_kind = StatusMessageKind::General;
                self.status_message_tone = StatusTone::Info;
            }
        }
        if let Some(status_time) = self.subscription_status_message_time {
            if status_time.elapsed() > Duration::from_secs(3) {
                self.subscription_status_message = None;
                self.subscription_status_message_time = None;
                self.subscription_status_message_tone = StatusTone::Info;
            }
        }

        match self.subscription.poll() {
            SubscriptionPoll::Batch(batch) => {
                let cache_file = self.product_paths.subscription_cache_file();
                self.install_subscription_batch(batch, |outputs| {
                    crate::subscription::cache::save(&cache_file, outputs)
                });
            }
            SubscriptionPoll::Disconnected => {
                self.subscription.install_disconnected();
                self.set_subscription_status_with_tone(
                    "Subscription fetch failed",
                    StatusTone::Danger,
                );
            }
            SubscriptionPoll::Pending => {}
        }
    }

    pub(super) fn advance_effective_date(&mut self, current_date: NaiveDate) {
        let previous_date = self.effective_date();
        if previous_date == current_date {
            return;
        }

        let selected_previous_today = self
            .selected_graph_cell
            .and_then(|cell| self.graph_date_for_cell(cell))
            .is_some_and(|date| date == previous_date);
        let query = UsageQuery {
            clients: self.local_usage.query().clients.clone(),
            group_by: self.local_usage.query().group_by,
            effective_date: current_date,
        };

        if self.has_installed_generation() {
            match self
                .local_usage
                .prepare_projection(query, self.detail_selections())
            {
                Ok(projection) => self.replace_usage_projection(projection, false),
                Err(error) => {
                    self.set_generation_status_with_tone(
                        &format!("Failed to advance local calendar: {error:#}"),
                        StatusTone::Danger,
                    );
                    return;
                }
            }
        } else {
            self.local_usage.replace_uninstalled_query(query);
        }

        if selected_previous_today {
            self.request_stats_today_selection();
        }
    }

    fn install_subscription_batch(
        &mut self,
        batch: SubscriptionBatch,
        persist: impl FnOnce(&[SubscriptionOutput]) -> anyhow::Result<()>,
    ) {
        match self.subscription.install(batch, persist) {
            SubscriptionInstall::Loaded => {
                self.set_subscription_status_with_tone(
                    "Subscription data loaded",
                    StatusTone::Success,
                );
            }
            SubscriptionInstall::LoadedWithErrors => self.set_subscription_status_with_tone(
                "Subscription data loaded with provider errors",
                StatusTone::Warning,
            ),
            SubscriptionInstall::Empty => self.set_subscription_status_with_tone(
                "No subscription data available",
                StatusTone::Warning,
            ),
            SubscriptionInstall::Failed => {
                self.set_subscription_status_with_tone(
                    "Subscription fetch failed",
                    StatusTone::Danger,
                );
            }
        }
    }

    pub(crate) fn handle_key_event(&mut self, key: KeyEvent) -> KeyEventOutcome {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return KeyEventOutcome::Exit(TuiExit::Interrupted);
        }

        if self.dialog_stack.is_active() {
            let result = self.dialog_stack.handle_key(key);
            self.handle_dialog_result(result);
            return KeyEventOutcome::Continue;
        }

        if let Some(command) = move_command_from_key(key.code) {
            if self.apply_text_viewport_move(command) {
                return KeyEventOutcome::Continue;
            }
        }

        match key.code {
            KeyCode::Char('q') => {
                return KeyEventOutcome::Exit(TuiExit::Quit);
            }
            KeyCode::Tab => {
                let next = self.next_visible_tab();
                self.switch_tab(next);
            }
            KeyCode::BackTab => {
                let prev = self.prev_visible_tab();
                self.switch_tab(prev);
            }
            KeyCode::Left => {
                let prev = self.prev_visible_tab();
                self.switch_tab(prev);
            }
            KeyCode::Right => {
                let next = self.next_visible_tab();
                self.switch_tab(next);
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
            KeyCode::Char('p') => {
                self.cycle_theme();
            }
            KeyCode::Char('r') if self.current_tab != Tab::Subscription => {
                self.refresh_requests.push_back(RefreshRequest::Manual);
                self.set_status(if self.is_background_loading() {
                    "Refresh queued"
                } else {
                    "Refresh requested"
                });
            }
            KeyCode::Char('R')
                if self.current_tab != Tab::Subscription
                    && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.refresh_controls
                    .push_back(RefreshControl::ToggleAutomatic);
            }
            KeyCode::Char('+') | KeyCode::Char('=') if self.current_tab != Tab::Subscription => {
                self.refresh_controls
                    .push_back(RefreshControl::IncreaseInterval);
            }
            KeyCode::Char('-') if self.current_tab != Tab::Subscription => {
                self.refresh_controls
                    .push_back(RefreshControl::DecreaseInterval);
            }
            KeyCode::Char('y') => {
                self.copy_selected_to_clipboard();
            }
            KeyCode::Char('e') if self.current_tab != Tab::Subscription => {
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
                self.reset_hourly_view_interaction();
            }
            KeyCode::Char('g') if self.group_by_applies_to_current_tab() => {
                self.open_group_by_picker();
            }
            KeyCode::Char('u') if self.current_tab == Tab::Subscription => {
                self.fetch_subscription();
            }
            KeyCode::Enter if self.current_tab == Tab::Models => {
                self.open_selected_model_detail();
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
                if self.current_tab == Tab::Models && self.is_model_detail_active() =>
            {
                self.close_model_detail();
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
            KeyCode::Esc | KeyCode::Backspace if self.selected_graph_cell.is_some() => {
                self.selected_graph_cell = None;
                self.stats_auto_select_today_pending = false;
                self.reset_current_list_interaction();
            }
            _ => {}
        }
        KeyEventOutcome::Continue
    }

    pub fn fetch_subscription(&mut self) {
        match self.subscription.request_fetch() {
            FetchRequest::AlreadyFetching => {
                self.set_subscription_status("Subscription fetch already in progress");
            }
            FetchRequest::NoProviders => self.set_subscription_status_with_tone(
                "No subscription providers enabled",
                StatusTone::Warning,
            ),
            FetchRequest::Started => {
                self.set_subscription_status("Fetching subscription...");
            }
        }
    }

    pub(crate) fn take_subscription_request(
        &mut self,
    ) -> Option<(Vec<ProviderId>, std::sync::mpsc::Sender<SubscriptionBatch>)> {
        self.subscription.take_request()
    }

    #[cfg(test)]
    fn start_subscription_fetch(&mut self, rx: std::sync::mpsc::Receiver<SubscriptionBatch>) {
        self.subscription.start_fetch_for_test(rx);
        self.set_subscription_status("Fetching subscription...");
    }

    fn should_start_initial_subscription_fetch(&self) -> bool {
        self.subscription.should_start_initial_fetch(
            self.current_tab == Tab::Subscription && self.settings.subscription.enabled,
        )
    }

    fn maybe_fetch_subscription_on_entry(&mut self) {
        if !self.should_start_initial_subscription_fetch() {
            return;
        }
        self.fetch_subscription();
    }

    #[cfg(test)]
    pub(crate) fn set_subscription_provider_ids_for_test(&mut self, ids: Vec<ProviderId>) {
        self.subscription.set_enabled_for_test(ids);
    }

    #[cfg(test)]
    pub(crate) fn start_subscription_fetch_for_test(
        &mut self,
        rx: std::sync::mpsc::Receiver<SubscriptionBatch>,
    ) {
        self.start_subscription_fetch(rx);
    }

    #[cfg(test)]
    pub(crate) fn replace_subscription_outputs_for_test(
        &mut self,
        outputs: Vec<SubscriptionOutput>,
    ) {
        self.subscription.replace_outputs_for_test(outputs);
    }

    #[cfg(test)]
    pub(crate) fn replace_subscription_errors_for_test(
        &mut self,
        errors: Vec<crate::subscription::SubscriptionError>,
    ) {
        self.subscription.replace_errors_for_test(errors);
    }

    #[cfg(test)]
    pub(crate) fn subscription_outputs_mut_for_test(&mut self) -> &mut Vec<SubscriptionOutput> {
        self.subscription.outputs_mut_for_test()
    }

    #[cfg(test)]
    pub(crate) fn set_last_subscription_check_for_test(&mut self, checked: Option<Instant>) {
        self.subscription.set_last_checked_for_test(checked);
    }

    pub fn is_fetching_subscription(&self) -> bool {
        self.subscription.is_fetching()
    }

    pub(crate) fn subscription_fetch_elapsed(&self) -> Option<Duration> {
        self.subscription.fetch_elapsed()
    }

    pub(crate) fn enabled_subscription_provider_count(&self) -> usize {
        self.subscription.enabled().len()
    }

    pub fn handle_mouse_event(&mut self, event: MouseEvent) {
        if self.dialog_stack.is_active() {
            let result = self.dialog_stack.handle_mouse(event);
            self.handle_dialog_result(result);
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
                            }
                            ClickAction::Sort(field) => {
                                self.set_sort(*field);
                            }
                            ClickAction::GraphCell { week, day } => {
                                self.selected_graph_cell = Some((*week, *day));
                                self.stats_auto_select_today_pending = false;
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

    pub(crate) fn set_subscription_text_viewport(&mut self, visible: usize, total_lines: usize) {
        self.subscription_text_total_lines = total_lines;
        self.subscription_viewport.set_visible(visible, total_lines);
    }

    pub(crate) fn subscription_text_visible_range(&self) -> std::ops::Range<usize> {
        self.subscription_viewport
            .visible_range(self.subscription_text_total_lines)
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
            Tab::Subscription => Some(&mut self.subscription_viewport),
            Tab::Hourly if self.hourly_view_mode == HourlyViewMode::Profile => {
                Some(&mut self.hourly_profile_viewport)
            }
            _ => None,
        }
    }

    fn current_text_total_lines(&self) -> Option<usize> {
        match self.current_tab {
            Tab::Subscription => Some(self.subscription_text_total_lines),
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

    fn current_list_interaction(&self) -> ListInteraction {
        ListInteraction {
            selected: self.selected_index,
            scroll: self.scroll_offset,
            visible: self.max_visible_items,
        }
    }

    fn set_current_list_interaction(&mut self, interaction: ListInteraction) {
        self.selected_index = interaction.selected;
        self.scroll_offset = interaction.scroll;
        self.max_visible_items = interaction.visible.max(1);
    }

    fn persist_list_interaction_for(&mut self, tab: Tab) {
        self.list_interactions
            .insert(tab, self.current_list_interaction());
    }

    fn should_persist_current_list_interaction(&self) -> bool {
        if self.current_tab == Tab::Models && self.is_model_detail_active() {
            return false;
        }
        if self.current_tab == Tab::Daily && self.is_daily_detail_active() {
            return false;
        }
        if self.current_period_kind().is_some() && self.is_period_detail_active() {
            return false;
        }
        true
    }

    fn persist_current_list_interaction(&mut self) {
        if self.should_persist_current_list_interaction() {
            self.persist_list_interaction_for(self.current_tab);
        }
    }

    fn stored_list_interaction(&self, tab: Tab) -> ListInteraction {
        let mut interaction = self
            .list_interactions
            .get(&tab)
            .copied()
            .unwrap_or_default();
        if interaction.visible == 0 {
            interaction.visible = self.max_visible_items.max(1);
        }
        interaction
    }

    fn restore_current_list_interaction(&mut self) {
        self.set_current_list_interaction(self.stored_list_interaction(self.current_tab));
        self.clamp_selection();
    }

    fn reset_current_list_interaction(&mut self) {
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.persist_current_list_interaction();
    }

    /// Clamp selection and scroll offset to valid bounds after data/resize changes.
    fn clamp_selection(&mut self) {
        let len = match self.get_current_list_len() {
            Ok(len) => len,
            Err(error) => {
                self.set_status_with_tone(
                    &format!("Usage view projection failed: {error:#}"),
                    StatusTone::Danger,
                );
                0
            }
        };
        let mut interaction = self.current_list_interaction();
        interaction.set_visible(self.max_visible_items, len);
        self.set_current_list_interaction(interaction);
        self.persist_current_list_interaction();
    }

    pub fn clear_click_areas(&mut self) {
        self.click_areas.clear();
    }

    pub fn add_click_area(&mut self, rect: Rect, action: ClickAction) {
        self.click_areas.push(ClickArea { rect, action });
    }

    fn reset_hourly_view_interaction(&mut self) {
        self.reset_current_list_interaction();
        self.hourly_profile_viewport.scroll = 0;
    }

    fn switch_tab(&mut self, target: Tab) {
        if !self.is_tab_visible(target) {
            return;
        }

        let entering_stats = target == Tab::Stats && self.current_tab != Tab::Stats;
        let was_model_detail = self.current_tab == Tab::Models && self.is_model_detail_active();
        let was_daily_detail = self.current_tab == Tab::Daily && self.is_daily_detail_active();
        let was_period_detail = self.is_period_detail_active();
        self.persist_current_sort();
        self.persist_current_list_interaction();

        self.current_tab = target;
        if was_model_detail {
            self.selected_model_detail = None;
            self.clear_detail_sort_context(DetailSortContextKind::Models);
        }
        if target != Tab::Daily || was_daily_detail {
            self.selected_daily_detail_date = None;
            self.clear_detail_sort_context(DetailSortContextKind::Daily);
        }
        if was_period_detail {
            self.selected_period_detail = None;
            self.clear_detail_sort_context(DetailSortContextKind::Period);
        }
        if target == Tab::Stats {
            // Re-clicking the already-active Stats tab must not discard a
            // manually chosen day; auto-select today only on tab entry.
            if entering_stats {
                self.request_stats_today_selection();
            }
        } else {
            self.selected_graph_cell = None;
            self.stats_auto_select_today_pending = false;
        }

        let (field, dir) = self
            .tab_sort_state
            .get(&target)
            .copied()
            .unwrap_or_else(|| Self::default_sort_for_tab(target));
        self.sort_field = field;
        self.sort_direction = dir;
        self.restore_current_list_interaction();
        self.maybe_fetch_subscription_on_entry();
    }

    fn default_sort_for_tab(tab: Tab) -> (SortField, SortDirection) {
        match tab {
            Tab::Models => (SortField::Tokens, SortDirection::Descending),
            Tab::Monthly | Tab::Weekly | Tab::Daily | Tab::Hourly => {
                (SortField::Date, SortDirection::Descending)
            }
            Tab::Overview | Tab::Subscription | Tab::Stats | Tab::Agents | Tab::Sessions => {
                (SortField::Cost, SortDirection::Descending)
            }
        }
    }

    fn default_sort_for_daily_detail() -> (SortField, SortDirection) {
        (SortField::Tokens, SortDirection::Descending)
    }

    fn default_sort_for_model_detail() -> (SortField, SortDirection) {
        (SortField::Tokens, SortDirection::Descending)
    }

    fn default_sort_for_period_detail() -> (SortField, SortDirection) {
        (SortField::Tokens, SortDirection::Descending)
    }

    pub(crate) fn tab_visible(settings: &Settings, tab: Tab) -> bool {
        match tab {
            Tab::Subscription => settings.subscription.enabled,
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
        if self.current_tab == Tab::Models && self.is_model_detail_active() {
            let context = self
                .detail_sort_contexts
                .entry(DetailSortContextKind::Models)
                .or_default();
            context.detail_sort_state = Some(current_sort);
            let model_sort = self
                .detail_sort_contexts
                .get(&DetailSortContextKind::Models)
                .and_then(|context| context.list_sort_before_detail)
                .unwrap_or_else(|| Self::default_sort_for_tab(Tab::Models));
            self.tab_sort_state.insert(Tab::Models, model_sort);
            return;
        }
        if self.current_tab == Tab::Daily && self.is_daily_detail_active() {
            let context = self
                .detail_sort_contexts
                .entry(DetailSortContextKind::Daily)
                .or_default();
            context.detail_sort_state = Some(current_sort);
            let daily_sort = self
                .detail_sort_contexts
                .get(&DetailSortContextKind::Daily)
                .and_then(|context| context.list_sort_before_detail)
                .unwrap_or_else(|| Self::default_sort_for_tab(Tab::Daily));
            self.tab_sort_state.insert(Tab::Daily, daily_sort);
            return;
        }
        if let Some(selection) = self.selected_period_detail {
            let context = self
                .detail_sort_contexts
                .entry(DetailSortContextKind::Period)
                .or_default();
            context.detail_sort_state = Some(current_sort);
            let tab = Self::period_tab(selection.kind);
            let period_sort = self
                .detail_sort_contexts
                .get(&DetailSortContextKind::Period)
                .and_then(|context| context.list_sort_before_detail)
                .unwrap_or_else(|| Self::default_sort_for_tab(tab));
            self.tab_sort_state.insert(tab, period_sort);
            return;
        }

        self.tab_sort_state.insert(self.current_tab, current_sort);
    }

    fn enter_detail_sort_context(
        &mut self,
        kind: DetailSortContextKind,
        default_detail_sort: fn() -> (SortField, SortDirection),
    ) {
        let list_sort = (self.sort_field, self.sort_direction);
        let context = self.detail_sort_contexts.entry(kind).or_default();
        context.list_sort_before_detail = Some(list_sort);
        let (field, direction) = self
            .detail_sort_contexts
            .get(&kind)
            .and_then(|context| context.detail_sort_state)
            .unwrap_or_else(default_detail_sort);
        self.sort_field = field;
        self.sort_direction = direction;
    }

    fn leave_detail_sort_context(&mut self, kind: DetailSortContextKind, tab: Tab) {
        let detail_sort = (self.sort_field, self.sort_direction);
        let saved_tab_sort = self.tab_sort_state.get(&tab).copied();
        let context = self.detail_sort_contexts.entry(kind).or_default();
        context.detail_sort_state = Some(detail_sort);
        let list_sort = context
            .list_sort_before_detail
            .take()
            .or(saved_tab_sort)
            .unwrap_or_else(|| Self::default_sort_for_tab(tab));
        self.sort_field = list_sort.0;
        self.sort_direction = list_sort.1;
        self.tab_sort_state.insert(tab, list_sort);
    }

    fn clear_detail_sort_context(&mut self, kind: DetailSortContextKind) {
        if let Some(context) = self.detail_sort_contexts.get_mut(&kind) {
            context.list_sort_before_detail = None;
        }
    }

    fn enter_daily_detail_sort_context(&mut self) {
        self.enter_detail_sort_context(
            DetailSortContextKind::Daily,
            Self::default_sort_for_daily_detail,
        );
    }

    fn enter_model_detail_sort_context(&mut self) {
        self.enter_detail_sort_context(
            DetailSortContextKind::Models,
            Self::default_sort_for_model_detail,
        );
    }

    fn leave_model_detail_sort_context(&mut self) {
        self.leave_detail_sort_context(DetailSortContextKind::Models, Tab::Models);
    }

    fn leave_daily_detail_sort_context(&mut self) {
        self.leave_detail_sort_context(DetailSortContextKind::Daily, Tab::Daily);
    }

    fn enter_period_detail_sort_context(&mut self) {
        self.enter_detail_sort_context(
            DetailSortContextKind::Period,
            Self::default_sort_for_period_detail,
        );
    }

    fn leave_period_detail_sort_context(&mut self) {
        let tab = self
            .selected_period_detail
            .map(|selection| Self::period_tab(selection.kind))
            .unwrap_or(self.current_tab);
        self.leave_detail_sort_context(DetailSortContextKind::Period, tab);
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
        let len = match self.get_current_list_len() {
            Ok(len) => len,
            Err(error) => {
                self.set_status_with_tone(
                    &format!("Usage view projection failed: {error:#}"),
                    StatusTone::Danger,
                );
                0
            }
        };
        let wrap = if self.current_tab == Tab::Stats {
            WrapMode::Clamp
        } else {
            WrapMode::Wrap
        };
        let mut interaction = self.current_list_interaction();
        let outcome = interaction.apply_move(command, len, wrap);
        self.set_current_list_interaction(interaction);
        self.persist_current_list_interaction();
        outcome
    }

    fn get_current_list_len(&self) -> Result<usize> {
        if self.current_tab.depends_on_local_generation() && !self.has_installed_generation() {
            return Ok(0);
        }

        Ok(match self.current_tab {
            Tab::Overview => self.usage().models.len(),
            Tab::Models if self.is_model_detail_active() => self.model_row_count(),
            Tab::Models => self.usage().models.len(),
            Tab::Agents => self.usage().agents.len(),
            Tab::Daily if self.is_daily_detail_active() => self.daily_detail_row_count(),
            Tab::Monthly if self.is_period_detail_active_for_kind(PeriodKind::Monthly) => {
                self.period_detail_row_count()
            }
            Tab::Weekly if self.is_period_detail_active_for_kind(PeriodKind::Weekly) => {
                self.period_detail_row_count()
            }
            Tab::Monthly => self.period_usage(PeriodKind::Monthly).len(),
            Tab::Weekly => self.period_usage(PeriodKind::Weekly).len(),
            Tab::Daily => self.usage().daily.len(),
            Tab::Hourly => self.usage().hourly.len(),
            Tab::Stats => 0,
            Tab::Subscription => self
                .subscription_outputs()
                .iter()
                .map(|u| u.metrics.len())
                .sum(),
            Tab::Sessions => 0,
        })
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
        if (self.current_tab == Tab::Models && self.is_model_detail_active())
            || (self.current_tab == Tab::Daily && self.is_daily_detail_active())
            || self.is_period_detail_active()
        {
            self.selected_index = 0;
            self.scroll_offset = 0;
        } else {
            self.selected_graph_cell = None;
            self.reset_current_list_interaction();
        }
        self.set_status(&format!(
            "Sorted by {:?} {:?}",
            self.sort_field, self.sort_direction
        ));
    }

    fn cycle_theme(&mut self) {
        let new_theme = self.theme.name.next();
        self.theme = Theme::from_name(new_theme);
        self.dialog_stack.set_theme(self.theme.clone());
        self.settings.set_theme(new_theme);
        if let Err(e) = self.settings.save(&self.product_paths) {
            self.set_status_with_tone(
                &format!("Theme: {} (save failed: {})", new_theme.as_str(), e),
                StatusTone::Danger,
            );
        } else {
            self.set_status(&format!("Theme: {}", new_theme.as_str()));
        }
    }

    fn open_client_picker(&mut self) {
        if !self.has_installed_generation() {
            self.set_status_with_tone(
                "Clients are unavailable until local data finishes loading",
                StatusTone::Warning,
            );
            return;
        }
        let mut clients: Vec<ClientId> = self.client_universe().iter().collect();
        clients.sort_by_key(|client| *client as usize);
        let dialog =
            ClientPickerDialog::new(clients, self.selected_clients().collect::<HashSet<_>>());
        self.dialog_stack.show(Box::new(dialog));
    }

    pub(crate) fn is_client_selected(&self, client: ClientId) -> bool {
        self.local_usage
            .query()
            .clients
            .iter()
            .any(|selected| selected == client)
    }

    /// Group By only reshapes the group-keyed projections (ADR 0010):
    /// Models plus the Daily/Monthly/Weekly tables built from them. The
    /// picker and its footer hint apply only on those tabs.
    pub fn group_by_applies_to_current_tab(&self) -> bool {
        matches!(
            self.current_tab,
            Tab::Models | Tab::Daily | Tab::Monthly | Tab::Weekly
        )
    }

    fn open_group_by_picker(&mut self) {
        if !self.has_installed_generation() {
            self.set_status_with_tone(
                "Group By is unavailable until local data finishes loading",
                StatusTone::Warning,
            );
            return;
        }
        use super::ui::dialog::GroupByPickerDialog;
        let dialog = GroupByPickerDialog::new(self.group_by());
        self.dialog_stack.show(Box::new(dialog));
    }

    pub fn model_details_supported(&self) -> bool {
        matches!(
            self.group_by(),
            tokenx_engine::GroupBy::Model | tokenx_engine::GroupBy::ClientModel
        )
    }

    pub fn is_model_detail_active(&self) -> bool {
        self.selected_model_detail.is_some()
    }

    fn model_detail_matches(selection: &ModelDetailSelection, model: &UsageModelEntry) -> bool {
        model.model_id.as_ref() == selection.model
            && selection
                .client
                .is_none_or(|client| model.clients.as_slice() == [client])
    }

    fn model_detail_update_for_clients(
        &mut self,
        projected_data: &UsageProjection,
        selected_clients: &HashSet<ClientId>,
    ) -> Result<ModelDetailClientUpdate> {
        let Some(selection) = self.selected_model_detail.clone() else {
            return Ok(ModelDetailClientUpdate::Inactive);
        };
        if !projected_data
            .models
            .iter()
            .any(|model| Self::model_detail_matches(&selection, model))
        {
            return Ok(ModelDetailClientUpdate::MissingSelection);
        }

        let detail_data = self.project_generation(
            tokenx_engine::GroupBy::ClientProviderModel,
            selected_clients,
        )?;
        if !detail_data
            .models
            .iter()
            .any(|model| Self::model_detail_matches(&selection, model))
        {
            anyhow::bail!("provider projection omitted the selected model");
        }

        Ok(ModelDetailClientUpdate::Ready(detail_data.models))
    }

    fn open_selected_model_detail(&mut self) {
        if self.is_model_detail_active() || !self.model_details_supported() {
            return;
        }

        let selection = self
            .model_at_sorted_index(self.selected_index)
            .map(|model| ModelDetailSelection {
                model: model.model_id.to_string(),
                client: (self.group_by() == tokenx_engine::GroupBy::ClientModel)
                    .then(|| model.clients.first().copied())
                    .flatten(),
            });
        let Some(selection) = selection else {
            return;
        };

        if self.model_detail_models.is_none() {
            let selected_clients = self.selected_clients().collect::<HashSet<_>>();
            if !self.has_installed_generation() {
                self.set_status_with_tone(
                    "Model details are unavailable until local data finishes loading",
                    StatusTone::Warning,
                );
                return;
            }
            let detail_data = match self.project_generation(
                tokenx_engine::GroupBy::ClientProviderModel,
                &selected_clients,
            ) {
                Ok(data) => data,
                Err(error) => {
                    self.set_status_with_tone(
                        &format!("Model details failed: {error:#}"),
                        StatusTone::Danger,
                    );
                    return;
                }
            };
            self.model_detail_models = Some(detail_data.models);
        }

        let has_rows = self.model_detail_models.as_deref().is_some_and(|models| {
            models
                .iter()
                .any(|model| Self::model_detail_matches(&selection, model))
        });
        if !has_rows {
            self.set_status_with_tone(
                "No provider details are available for the selected model",
                StatusTone::Warning,
            );
            return;
        }

        self.persist_list_interaction_for(Tab::Models);
        self.selected_model_detail = Some(selection.clone());
        self.enter_model_detail_sort_context();
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.set_generation_status(&format!("Viewing provider details for {}", selection.model));
        self.clamp_selection();
    }

    fn clear_model_detail_state(&mut self, invalidate_projection: bool) {
        if self.is_model_detail_active() {
            self.leave_model_detail_sort_context();
            self.selected_model_detail = None;
            self.set_current_list_interaction(self.stored_list_interaction(Tab::Models));
        }
        if invalidate_projection {
            self.model_detail_models = None;
        }
    }

    fn close_model_detail(&mut self) {
        let Some(selection) = self.selected_model_detail.clone() else {
            return;
        };

        self.leave_model_detail_sort_context();
        self.selected_model_detail = None;

        let restored_index = self
            .model_render_order()
            .iter()
            .position(|index| {
                self.model_at_source_index(*index)
                    .is_some_and(|model| Self::model_detail_matches(&selection, model))
            })
            .unwrap_or_else(|| self.stored_list_interaction(Tab::Models).selected);
        let model_interaction = self.stored_list_interaction(Tab::Models);
        let max_visible = model_interaction.visible.max(1);
        let viewport_still_holds = restored_index >= model_interaction.scroll
            && restored_index < model_interaction.scroll + max_visible;
        let scroll = if viewport_still_holds {
            model_interaction.scroll
        } else {
            restored_index.saturating_sub(max_visible / 2)
        };
        self.set_current_list_interaction(ListInteraction {
            selected: restored_index,
            scroll,
            visible: model_interaction.visible,
        });

        self.set_generation_status(&format!("Returned to model {}", selection.model));
        self.clamp_selection();
    }

    fn open_selected_daily_detail(&mut self) {
        if self.is_daily_detail_active() {
            return;
        }

        let selected_date = self
            .daily_at_sorted_index(self.selected_index)
            .map(|day| day.date);

        if let Some(date) = selected_date {
            if let Err(error) = self.local_usage.materialize_daily_detail(date) {
                self.set_generation_status_with_tone(
                    &format!("Daily detail projection failed: {error:#}"),
                    StatusTone::Danger,
                );
                return;
            }
            self.persist_list_interaction_for(Tab::Daily);
            self.selected_daily_detail_date = Some(date);
            self.enter_daily_detail_sort_context();
            self.selected_index = 0;
            self.scroll_offset = 0;
            self.set_generation_status(&format!("Viewing daily details for {}", date));
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
            .daily_render_order()
            .iter()
            .position(|index| self.usage().daily[*index].date == detail_date)
            .unwrap_or_else(|| self.stored_list_interaction(Tab::Daily).selected);

        let daily_interaction = self.stored_list_interaction(Tab::Daily);
        let max_visible = daily_interaction.visible.max(1);
        let viewport_still_holds = restored_index >= daily_interaction.scroll
            && restored_index < daily_interaction.scroll + max_visible;
        let scroll = if viewport_still_holds {
            daily_interaction.scroll
        } else {
            restored_index.saturating_sub(max_visible / 2)
        };
        self.set_current_list_interaction(ListInteraction {
            selected: restored_index,
            scroll,
            visible: daily_interaction.visible,
        });

        self.set_generation_status("Returned to daily usage");
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
            if let Err(error) = self.local_usage.materialize_period_detail(selection) {
                self.set_generation_status_with_tone(
                    &format!("Period detail projection failed: {error:#}"),
                    StatusTone::Danger,
                );
                return;
            }
            self.persist_list_interaction_for(Self::period_tab(kind));
            self.selected_period_detail = Some(selection);
            self.enter_period_detail_sort_context();
            self.selected_index = 0;
            self.scroll_offset = 0;
            self.set_generation_status(&format!("Viewing period details for {}", label));
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
            .unwrap_or_else(|| {
                self.stored_list_interaction(Self::period_tab(selection.kind))
                    .selected
            });

        let period_interaction = self.stored_list_interaction(Self::period_tab(selection.kind));
        let max_visible = period_interaction.visible.max(1);
        let viewport_still_holds = restored_index >= period_interaction.scroll
            && restored_index < period_interaction.scroll + max_visible;
        let scroll = if viewport_still_holds {
            period_interaction.scroll
        } else {
            restored_index.saturating_sub(max_visible / 2)
        };
        self.set_current_list_interaction(ListInteraction {
            selected: restored_index,
            scroll,
            visible: period_interaction.visible,
        });

        self.set_generation_status(match selection.kind {
            PeriodKind::Monthly => "Returned to monthly usage",
            PeriodKind::Weekly => "Returned to weekly usage",
        });
        self.clamp_selection();
    }

    fn selected_copy_text(&self) -> Option<String> {
        match self.current_tab {
            Tab::Overview | Tab::Models => {
                self.model_at_sorted_index(self.selected_index).map(|m| {
                    format!(
                        "{}: {} tokens, ${:.4}",
                        m.display_name,
                        m.tokens.total(),
                        m.cost
                    )
                })
            }
            Tab::Agents => self.get_sorted_agents().get(self.selected_index).map(|a| {
                format!(
                    "{} / {}: {} tokens, ${:.4}, {} instances",
                    a.client,
                    a.agent,
                    a.tokens.total(),
                    a.cost,
                    a.instance_count
                )
            }),
            Tab::Daily if self.is_daily_detail_active() => self
                .daily_detail_rows()
                .get(self.selected_index)
                .map(|row| {
                    format!(
                        "{} / {}: {} tokens, ${:.4}",
                        client_ids_text(&row.clients),
                        row.model,
                        row.tokens.total(),
                        row.cost
                    )
                }),
            Tab::Monthly | Tab::Weekly if self.is_period_detail_active() => self
                .period_detail_rows()
                .get(self.selected_index)
                .map(|row| {
                    format!(
                        "{} / {}: {} tokens, ${:.4}",
                        client_ids_text(&row.clients),
                        row.model,
                        row.tokens.total(),
                        row.cost
                    )
                }),
            Tab::Daily => self
                .daily_at_sorted_index(self.selected_index)
                .map(|d| format!("{}: {} tokens, ${:.4}", d.date, d.tokens.total(), d.cost)),
            Tab::Monthly => self
                .get_sorted_periods(PeriodKind::Monthly)
                .get(self.selected_index)
                .copied()
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
                .copied()
                .map(|p| {
                    format!(
                        "{} {}: {} tokens, ${:.4}",
                        p.section_label,
                        p.label,
                        p.tokens.total(),
                        p.cost
                    )
                }),
            Tab::Hourly => self.hourly_at_sorted_index(self.selected_index).map(|h| {
                format!(
                    "{}: {} tokens, ${:.4}",
                    h.datetime.format("%Y-%m-%d %H:%M"),
                    h.tokens.total(),
                    h.cost
                )
            }),
            Tab::Stats | Tab::Subscription | Tab::Sessions => None,
        }
    }

    fn copy_selected_to_clipboard(&mut self) {
        let text = self.selected_copy_text();

        if let Some(text) = text {
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&text)) {
                Ok(_) => self.set_status_with_tone("Copied to clipboard", StatusTone::Success),
                Err(_) => self.set_status_with_tone("Failed to copy", StatusTone::Danger),
            }
        }
    }

    fn export_group_by(&self) -> tokenx_engine::GroupBy {
        self.group_by()
    }

    fn export_to_json(&mut self) {
        let filename = format!(
            "tokenx-export-{}.json",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );
        let export_dir = self.product_paths.export_dir();
        let path = export_dir.join(filename);
        let group_by = self.export_group_by();

        let Some(health) = self.generation_health() else {
            self.set_status_with_tone(
                "Export failed: local data is not loaded yet",
                StatusTone::Danger,
            );
            return;
        };
        let health = health.clone();

        match crate::report::build_usage_report_json(self.usage(), &health, &group_by) {
            Ok(json) => {
                match std::fs::create_dir_all(&export_dir).and_then(|_| std::fs::write(&path, json))
                {
                    Ok(_) => self.set_status_with_tone(
                        &format!("Exported to {}", path.display()),
                        StatusTone::Success,
                    ),
                    Err(e) => self
                        .set_status_with_tone(&format!("Export failed: {}", e), StatusTone::Danger),
                }
            }
            Err(e) => {
                self.set_status_with_tone(&format!("Export failed: {}", e), StatusTone::Danger)
            }
        }
    }

    fn handle_graph_selection(&mut self) {
        if self.current_tab == Tab::Stats && self.selected_graph_cell.is_some() {
            self.set_status("Press ESC to deselect");
        }
    }

    pub fn set_status(&mut self, message: &str) {
        self.set_status_with_tone(message, StatusTone::Info);
    }

    pub(crate) fn set_status_with_tone(&mut self, message: &str, tone: StatusTone) {
        self.status_message = Some(message.to_string());
        self.status_message_time = Some(Instant::now());
        self.status_message_kind = StatusMessageKind::General;
        self.status_message_tone = tone;
    }

    pub(crate) fn set_generation_status(&mut self, message: &str) {
        self.set_generation_status_with_tone(message, StatusTone::Info);
    }

    pub(crate) fn set_generation_status_with_tone(&mut self, message: &str, tone: StatusTone) {
        self.status_message = Some(message.to_string());
        self.status_message_time = Some(Instant::now());
        self.status_message_kind = StatusMessageKind::Generation;
        self.status_message_tone = tone;
    }

    fn set_subscription_status(&mut self, message: &str) {
        self.set_subscription_status_with_tone(message, StatusTone::Info);
    }

    pub(crate) fn set_subscription_status_with_tone(&mut self, message: &str, tone: StatusTone) {
        let now = Instant::now();
        let message = message.to_string();
        self.subscription_status_message = Some(message);
        self.subscription_status_message_time = Some(now);
        self.subscription_status_message_tone = tone;
    }

    pub fn general_status_message(&self) -> Option<&str> {
        if self.status_message_kind == StatusMessageKind::General {
            self.status_message.as_deref()
        } else {
            None
        }
    }

    pub(crate) fn status_message_tone(&self) -> StatusTone {
        self.status_message_tone
    }

    pub(crate) fn subscription_status_message_tone(&self) -> StatusTone {
        self.subscription_status_message_tone
    }

    fn model_order_source(&self) -> &[UsageModelEntry] {
        if self.selected_model_detail.is_some() {
            self.model_detail_models.as_deref().unwrap_or_default()
        } else {
            &self.usage().models
        }
    }

    pub(crate) fn model_render_order(&self) -> Arc<[usize]> {
        let key = ModelOrderKey {
            usage_revision: self.usage_revision,
            sort_field: self.sort_field,
            sort_direction: self.sort_direction,
            detail: self.selected_model_detail.clone(),
        };
        if let Some(cached) = self
            .render_order_cache
            .borrow()
            .models
            .as_ref()
            .filter(|cached| cached.key == key)
        {
            return Arc::clone(&cached.order);
        }

        let source = self.model_order_source();
        let mut order = match self.selected_model_detail.as_ref() {
            Some(selection) => source
                .iter()
                .enumerate()
                .filter_map(|(index, model)| {
                    Self::model_detail_matches(selection, model).then_some(index)
                })
                .collect::<Vec<_>>(),
            None => (0..source.len()).collect::<Vec<_>>(),
        };

        let tie_breaker = |a: &UsageModelEntry, b: &UsageModelEntry| {
            a.model_id
                .cmp(&b.model_id)
                .then_with(|| a.workspace_label.cmp(&b.workspace_label))
                .then_with(|| a.workspace_key.cmp(&b.workspace_key))
                .then_with(|| a.provider.cmp(&b.provider))
                .then_with(|| a.clients.cmp(&b.clients))
        };

        match (self.sort_field, self.sort_direction) {
            (SortField::Cost, SortDirection::Descending) => order.sort_by(|a, b| {
                let a = &source[*a];
                let b = &source[*b];
                b.cost.total_cmp(&a.cost).then_with(|| tie_breaker(a, b))
            }),
            (SortField::Cost, SortDirection::Ascending) => order.sort_by(|a, b| {
                let a = &source[*a];
                let b = &source[*b];
                a.cost.total_cmp(&b.cost).then_with(|| tie_breaker(a, b))
            }),
            (SortField::Tokens, SortDirection::Descending) => order.sort_by(|a, b| {
                let a = &source[*a];
                let b = &source[*b];
                b.tokens
                    .total()
                    .cmp(&a.tokens.total())
                    .then_with(|| tie_breaker(a, b))
            }),
            (SortField::Tokens, SortDirection::Ascending) => order.sort_by(|a, b| {
                let a = &source[*a];
                let b = &source[*b];
                a.tokens
                    .total()
                    .cmp(&b.tokens.total())
                    .then_with(|| tie_breaker(a, b))
            }),
            (SortField::Date, _) => {
                order.sort_by(|a, b| tie_breaker(&source[*a], &source[*b]));
            }
        }

        let order: Arc<[usize]> = order.into();
        self.render_order_cache.borrow_mut().models = Some(CachedRenderOrder {
            key,
            order: Arc::clone(&order),
        });
        order
    }

    pub(crate) fn model_at_source_index(&self, index: usize) -> Option<&UsageModelEntry> {
        self.model_order_source().get(index)
    }

    pub(crate) fn model_at_sorted_index(&self, index: usize) -> Option<&UsageModelEntry> {
        let order = self.model_render_order();
        self.model_at_source_index(*order.get(index)?)
    }

    pub(crate) fn model_row_count(&self) -> usize {
        match self.selected_model_detail.as_ref() {
            Some(selection) => self
                .model_order_source()
                .iter()
                .filter(|model| Self::model_detail_matches(selection, model))
                .count(),
            None => self.usage().models.len(),
        }
    }

    #[cfg(test)]
    pub fn get_sorted_models(&self) -> Vec<&UsageModelEntry> {
        self.model_render_order()
            .iter()
            .map(|index| {
                self.model_at_source_index(*index)
                    .expect("cached model index must reference the current projection")
            })
            .collect()
    }

    pub fn get_sorted_agents(&self) -> Vec<&AgentEntry> {
        let mut agents: Vec<&AgentEntry> = self.usage().agents.iter().collect();

        let tie_breaker = |a: &&AgentEntry, b: &&AgentEntry| {
            a.agent.cmp(&b.agent).then_with(|| a.client.cmp(&b.client))
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

    pub(crate) fn daily_render_order(&self) -> Arc<[usize]> {
        let key = UsageOrderKey {
            usage_revision: self.usage_revision,
            sort_field: self.sort_field,
            sort_direction: self.sort_direction,
        };
        if let Some(cached) = self
            .render_order_cache
            .borrow()
            .daily
            .as_ref()
            .filter(|cached| cached.key == key)
        {
            return Arc::clone(&cached.order);
        }

        let daily = &self.usage().daily;
        let mut order = (0..daily.len()).collect::<Vec<_>>();

        match (self.sort_field, self.sort_direction) {
            (SortField::Cost, SortDirection::Descending) => order.sort_by(|a, b| {
                let a = &daily[*a];
                let b = &daily[*b];
                b.cost.total_cmp(&a.cost).then_with(|| a.date.cmp(&b.date))
            }),
            (SortField::Cost, SortDirection::Ascending) => order.sort_by(|a, b| {
                let a = &daily[*a];
                let b = &daily[*b];
                a.cost.total_cmp(&b.cost).then_with(|| a.date.cmp(&b.date))
            }),
            (SortField::Tokens, SortDirection::Descending) => order.sort_by(|a, b| {
                let a = &daily[*a];
                let b = &daily[*b];
                b.tokens
                    .total()
                    .cmp(&a.tokens.total())
                    .then_with(|| a.date.cmp(&b.date))
            }),
            (SortField::Tokens, SortDirection::Ascending) => order.sort_by(|a, b| {
                let a = &daily[*a];
                let b = &daily[*b];
                a.tokens
                    .total()
                    .cmp(&b.tokens.total())
                    .then_with(|| a.date.cmp(&b.date))
            }),
            (SortField::Date, SortDirection::Descending) => {
                order.sort_by_key(|index| std::cmp::Reverse(daily[*index].date))
            }
            (SortField::Date, SortDirection::Ascending) => {
                order.sort_by_key(|index| daily[*index].date)
            }
        }

        let order: Arc<[usize]> = order.into();
        self.render_order_cache.borrow_mut().daily = Some(CachedRenderOrder {
            key,
            order: Arc::clone(&order),
        });
        order
    }

    pub(crate) fn daily_at_sorted_index(&self, index: usize) -> Option<&DailyUsage> {
        let order = self.daily_render_order();
        self.usage().daily.get(*order.get(index)?)
    }

    #[cfg(test)]
    pub fn get_sorted_daily(&self) -> Vec<&DailyUsage> {
        self.daily_render_order()
            .iter()
            .map(|index| &self.usage().daily[*index])
            .collect()
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
        self.period_usage(selection.kind)
            .iter()
            .find(|period| {
                period.start_date == selection.start_date && period.end_date == selection.end_date
            })
            .map(|period| format!("{} {}", period.section_label, period.label))
    }

    fn detail_render_order(
        &self,
        selection: DetailOrderSelection,
        rows: &[DetailRow],
    ) -> Arc<[usize]> {
        let key = DetailOrderKey {
            usage_revision: self.usage_revision,
            selection,
            sort_field: self.sort_field,
            sort_direction: self.sort_direction,
        };
        if let Some(cached) = self
            .render_order_cache
            .borrow()
            .detail
            .as_ref()
            .filter(|cached| cached.key == key)
        {
            return Arc::clone(&cached.order);
        }

        let mut order = (0..rows.len()).collect::<Vec<_>>();
        sort_detail_order(&mut order, rows, self.sort_field, self.sort_direction);
        let order: Arc<[usize]> = order.into();
        self.render_order_cache.borrow_mut().detail = Some(CachedRenderOrder {
            key,
            order: Arc::clone(&order),
        });
        order
    }

    pub(crate) fn daily_detail_rows(&self) -> OrderedDetailRows<'_> {
        let Some(date) = self.selected_daily_detail_date else {
            return OrderedDetailRows {
                rows: &[],
                order: Arc::from([]),
            };
        };
        let rows = self.require_installed_generation().daily_detail(date);
        OrderedDetailRows {
            rows,
            order: self.detail_render_order(DetailOrderSelection::Daily(date), rows),
        }
    }

    pub(crate) fn daily_detail_row_count(&self) -> usize {
        let Some(date) = self.selected_daily_detail_date else {
            return 0;
        };
        self.require_installed_generation().daily_detail(date).len()
    }

    pub(crate) fn period_detail_rows(&self) -> OrderedDetailRows<'_> {
        let Some(selection) = self.selected_period_detail else {
            return OrderedDetailRows {
                rows: &[],
                order: Arc::from([]),
            };
        };
        let rows = self.require_installed_generation().period_detail(selection);
        OrderedDetailRows {
            rows,
            order: self.detail_render_order(DetailOrderSelection::Period(selection), rows),
        }
    }

    pub(crate) fn period_detail_row_count(&self) -> usize {
        let Some(selection) = self.selected_period_detail else {
            return 0;
        };
        self.require_installed_generation()
            .period_detail(selection)
            .len()
    }

    pub(crate) fn hourly_render_order(&self) -> Arc<[usize]> {
        let key = UsageOrderKey {
            usage_revision: self.usage_revision,
            sort_field: self.sort_field,
            sort_direction: self.sort_direction,
        };
        if let Some(cached) = self
            .render_order_cache
            .borrow()
            .hourly
            .as_ref()
            .filter(|cached| cached.key == key)
        {
            return Arc::clone(&cached.order);
        }

        let hourly = &self.usage().hourly;
        let mut order = (0..hourly.len()).collect::<Vec<_>>();

        match (self.sort_field, self.sort_direction) {
            (SortField::Cost, SortDirection::Descending) => order.sort_by(|a, b| {
                let a = &hourly[*a];
                let b = &hourly[*b];
                b.cost
                    .total_cmp(&a.cost)
                    .then_with(|| a.datetime.cmp(&b.datetime))
            }),
            (SortField::Cost, SortDirection::Ascending) => order.sort_by(|a, b| {
                let a = &hourly[*a];
                let b = &hourly[*b];
                a.cost
                    .total_cmp(&b.cost)
                    .then_with(|| a.datetime.cmp(&b.datetime))
            }),
            (SortField::Tokens, SortDirection::Descending) => order.sort_by(|a, b| {
                let a = &hourly[*a];
                let b = &hourly[*b];
                b.tokens
                    .total()
                    .cmp(&a.tokens.total())
                    .then_with(|| a.datetime.cmp(&b.datetime))
            }),
            (SortField::Tokens, SortDirection::Ascending) => order.sort_by(|a, b| {
                let a = &hourly[*a];
                let b = &hourly[*b];
                a.tokens
                    .total()
                    .cmp(&b.tokens.total())
                    .then_with(|| a.datetime.cmp(&b.datetime))
            }),
            (SortField::Date, SortDirection::Descending) => {
                order.sort_by_key(|index| std::cmp::Reverse(hourly[*index].datetime))
            }
            (SortField::Date, SortDirection::Ascending) => {
                order.sort_by_key(|index| hourly[*index].datetime)
            }
        }

        let order: Arc<[usize]> = order.into();
        self.render_order_cache.borrow_mut().hourly = Some(CachedRenderOrder {
            key,
            order: Arc::clone(&order),
        });
        order
    }

    pub(crate) fn hourly_at_sorted_index(&self, index: usize) -> Option<&HourlyUsage> {
        let order = self.hourly_render_order();
        self.usage().hourly.get(*order.get(index)?)
    }

    pub fn get_sorted_periods(&self, kind: PeriodKind) -> Vec<&PeriodUsage> {
        let mut periods = self.period_usage(kind).iter().collect::<Vec<_>>();

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
    use super::*;
    use crate::acquisition::{acquisition_engine, build_generation};
    use crate::tui::data::{DailyClientInfo, DailyModelInfo, UsageModelEntry, UsageTokenBreakdown};
    use chrono::NaiveDate;
    use serial_test::serial;
    use std::collections::{BTreeMap, BTreeSet};

    type ClientModelCosts<'a> = Vec<(&'a str, Vec<(&'a str, &'a str, f64)>)>;

    fn config_with_theme(theme: Option<&str>) -> TuiConfig {
        TuiConfig {
            theme: theme.map(|value| value.parse().unwrap()),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        }
    }

    fn cached_order_model(id: &str, tokens: u64) -> UsageModelEntry {
        UsageModelEntry {
            model_id: id.into(),
            display_name: id.into(),
            provider: "openai".into(),
            clients: vec![ClientId::Codex],
            tokens: UsageTokenBreakdown {
                input: tokens,
                ..UsageTokenBreakdown::default()
            },
            cost: tokens as f64,
            session_count: 1,
            workspace_key: None,
            workspace_label: None,
        }
    }

    #[test]
    fn render_order_cache_reuses_matching_keys_and_invalidates_semantic_changes() {
        let mut app = App::new_for_test(config_with_theme(None)).unwrap();
        app.usage_mut_for_test().models = vec![
            cached_order_model("slow", 1),
            cached_order_model("fast", 10),
        ];

        let first = app.model_render_order();
        let repeated = app.model_render_order();
        assert!(Arc::ptr_eq(&first, &repeated));

        app.sort_direction = SortDirection::Ascending;
        let resorted = app.model_render_order();
        assert!(!Arc::ptr_eq(&first, &resorted));

        app.model_detail_models = Some(vec![
            cached_order_model("fast", 3),
            cached_order_model("other", 4),
        ]);
        app.selected_model_detail = Some(ModelDetailSelection {
            model: "fast".to_string(),
            client: None,
        });
        let detail = app.model_render_order();
        assert!(!Arc::ptr_eq(&resorted, &detail));
        assert_eq!(detail.len(), 1);

        app.selected_model_detail = None;
        app.usage_mut_for_test().models[0].cost = 99.0;
        let replaced = app.model_render_order();
        assert!(!Arc::ptr_eq(&resorted, &replaced));
    }

    #[test]
    fn daily_and_hourly_orders_share_only_an_identical_revision_and_sort_key() {
        let mut app = App::new_for_test(config_with_theme(None)).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 7, 26).unwrap();
        app.usage_mut_for_test().daily = vec![DailyUsage {
            date,
            tokens: UsageTokenBreakdown::default(),
            cost: 1.0,
            client_breakdown: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
        }];
        app.usage_mut_for_test().hourly = vec![HourlyUsage {
            datetime: date.and_hms_opt(10, 0, 0).unwrap(),
            tokens: UsageTokenBreakdown::default(),
            cost: 1.0,
            clients: BTreeSet::new(),
            models: Vec::new(),
            message_count: 0,
            turn_count: 0,
        }];

        let daily = app.daily_render_order();
        let hourly = app.hourly_render_order();
        assert!(Arc::ptr_eq(&daily, &app.daily_render_order()));
        assert!(Arc::ptr_eq(&hourly, &app.hourly_render_order()));

        app.sort_field = SortField::Tokens;
        assert!(!Arc::ptr_eq(&daily, &app.daily_render_order()));
        assert!(!Arc::ptr_eq(&hourly, &app.hourly_render_order()));

        let before_revision = app.usage_revision;
        app.update_data(UsageProjection::default());
        assert_ne!(app.usage_revision, before_revision);
        assert!(app.daily_render_order().is_empty());
        assert!(app.hourly_render_order().is_empty());
    }

    #[test]
    fn saved_theme_is_used_when_cli_theme_is_absent() {
        let settings = Settings {
            color_palette: ThemeName::Lagoon,
            ..Settings::default()
        };

        let app = App::new_for_test_with_settings(config_with_theme(None), settings).unwrap();

        assert_eq!(app.theme.name, ThemeName::Lagoon);
    }

    #[test]
    fn explicit_cli_theme_overrides_saved_theme() {
        let settings = Settings {
            color_palette: ThemeName::Lagoon,
            ..Settings::default()
        };

        let app =
            App::new_for_test_with_settings(config_with_theme(Some("graphite")), settings).unwrap();

        assert_eq!(app.theme.name, ThemeName::Graphite);
    }

    #[test]
    fn app_consumes_the_passed_settings_snapshot_without_reading_disk() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("settings.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        let settings = Settings {
            color_palette: ThemeName::Lagoon,
            auto_refresh_enabled: true,
            auto_refresh_ms: 40_000,
            ..Settings::default()
        };

        let app = App::new_for_test_with_settings_and_paths(
            config_with_theme(None),
            settings,
            ProductPaths::at(temp.path()),
        )
        .unwrap();

        assert_eq!(app.theme.name, ThemeName::Lagoon);
        assert!(app.auto_refresh_enabled());
        assert_eq!(app.auto_refresh_interval(), Duration::from_secs(40));
    }

    #[test]
    fn test_tab_all() {
        let tabs = Tab::all();
        assert_eq!(tabs.len(), 10);
        assert_eq!(tabs[0], Tab::Overview);
        assert_eq!(tabs[1], Tab::Subscription);
        assert_eq!(tabs[2], Tab::Models);
        assert_eq!(tabs[3], Tab::Monthly);
        assert_eq!(tabs[4], Tab::Weekly);
        assert_eq!(tabs[5], Tab::Daily);
        assert_eq!(tabs[6], Tab::Hourly);
        assert_eq!(tabs[7], Tab::Stats);
        assert_eq!(tabs[8], Tab::Agents);
        assert_eq!(tabs[9], Tab::Sessions);
    }

    #[test]
    fn test_tab_next() {
        assert_eq!(Tab::Overview.next(), Tab::Subscription);
        assert_eq!(Tab::Subscription.next(), Tab::Models);
        assert_eq!(Tab::Models.next(), Tab::Monthly);
        assert_eq!(Tab::Monthly.next(), Tab::Weekly);
        assert_eq!(Tab::Weekly.next(), Tab::Daily);
        assert_eq!(Tab::Daily.next(), Tab::Hourly);
        assert_eq!(Tab::Hourly.next(), Tab::Stats);
        assert_eq!(Tab::Stats.next(), Tab::Agents);
        assert_eq!(Tab::Agents.next(), Tab::Sessions);
        assert_eq!(Tab::Sessions.next(), Tab::Overview);
    }

    #[test]
    fn test_tab_prev() {
        assert_eq!(Tab::Overview.prev(), Tab::Sessions);
        assert_eq!(Tab::Subscription.prev(), Tab::Overview);
        assert_eq!(Tab::Models.prev(), Tab::Subscription);
        assert_eq!(Tab::Monthly.prev(), Tab::Models);
        assert_eq!(Tab::Weekly.prev(), Tab::Monthly);
        assert_eq!(Tab::Daily.prev(), Tab::Weekly);
        assert_eq!(Tab::Hourly.prev(), Tab::Daily);
        assert_eq!(Tab::Stats.prev(), Tab::Hourly);
        assert_eq!(Tab::Agents.prev(), Tab::Stats);
        assert_eq!(Tab::Sessions.prev(), Tab::Agents);
    }

    #[test]
    fn test_tab_as_str() {
        assert_eq!(Tab::Overview.as_str(), "Overview");
        assert_eq!(Tab::Subscription.as_str(), "Subscription");
        assert_eq!(Tab::Models.as_str(), "Models");
        assert_eq!(Tab::Agents.as_str(), "Agents");
        assert_eq!(Tab::Monthly.as_str(), "Monthly");
        assert_eq!(Tab::Weekly.as_str(), "Weekly");
        assert_eq!(Tab::Daily.as_str(), "Daily");
        assert_eq!(Tab::Hourly.as_str(), "Hourly");
        assert_eq!(Tab::Stats.as_str(), "Stats");
        assert_eq!(Tab::Sessions.as_str(), "Sessions");
    }

    #[test]
    fn test_tab_short_name() {
        assert_eq!(Tab::Overview.short_name(), "Ovw");
        assert_eq!(Tab::Subscription.short_name(), "Sub");
        assert_eq!(Tab::Models.short_name(), "Mod");
        assert_eq!(Tab::Agents.short_name(), "Agt");
        assert_eq!(Tab::Monthly.short_name(), "Mth");
        assert_eq!(Tab::Weekly.short_name(), "Wk");
        assert_eq!(Tab::Daily.short_name(), "Day");
        assert_eq!(Tab::Hourly.short_name(), "Hr");
        assert_eq!(Tab::Stats.short_name(), "Sta");
        assert_eq!(Tab::Sessions.short_name(), "Ses");
    }

    #[test]
    fn test_move_selection_up() {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let mut app = App::new_for_test(config).unwrap();

        // Add some mock data
        app.usage_mut_for_test().models = vec![
            UsageModelEntry {
                model_id: "model1".into(),
                display_name: "model1".into(),
                provider: "provider1".into(),
                clients: vec![ClientId::OpenCode],
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
            UsageModelEntry {
                model_id: "model2".into(),
                display_name: "model2".into(),
                provider: "provider2".into(),
                clients: vec![ClientId::OpenCode],
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
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
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let mut app = App::new_for_test(config).unwrap();

        // Add some mock data
        app.usage_mut_for_test().models = vec![
            UsageModelEntry {
                model_id: "model1".into(),
                display_name: "model1".into(),
                provider: "provider1".into(),
                clients: vec![ClientId::OpenCode],
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
            UsageModelEntry {
                model_id: "model2".into(),
                display_name: "model2".into(),
                provider: "provider2".into(),
                clients: vec![ClientId::OpenCode],
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
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
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let mut app = App::new_for_test(config).unwrap();

        // Add some mock data
        app.usage_mut_for_test().models = vec![UsageModelEntry {
            model_id: "model1".into(),
            display_name: "model1".into(),
            provider: "provider1".into(),
            clients: vec![ClientId::OpenCode],
            tokens: UsageTokenBreakdown::default(),
            cost: 0.0,
            session_count: 1,
            workspace_key: None,
            workspace_label: None,
        }];

        // Set selection beyond bounds
        app.selected_index = 10;
        app.clamp_selection();
        assert_eq!(app.selected_index, 0);

        // Empty data
        app.usage_mut_for_test().models.clear();
        app.selected_index = 5;
        app.clamp_selection();
        assert_eq!(app.selected_index, 0);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_set_sort() {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let mut app = App::new_for_test(config).unwrap();

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

    // ── Helper ──────────────────────────────────────────────────────

    fn test_settings() -> Settings {
        Settings::default()
    }

    fn load_test_generation() -> (tempfile::TempDir, tokenx_engine::Generation) {
        let home = tempfile::TempDir::new().unwrap();
        for (project, workspace, input_tokens) in
            [("project-a", "/work/a", 10), ("project-b", "/work/b", 20)]
        {
            let project_dir = home.path().join(".claude/projects").join(project);
            std::fs::create_dir_all(&project_dir).unwrap();
            std::fs::write(
                project_dir.join("session.jsonl"),
                format!(
                    r#"{{"type":"assistant","timestamp":"2026-05-27T10:00:00.000Z","cwd":"{workspace}","requestId":"request-{project}","message":{{"id":"message-{project}","model":"claude-sonnet-4.6","usage":{{"input_tokens":{input_tokens},"output_tokens":1}}}}}}"#
                ),
            )
            .unwrap();
        }
        let acquisition = acquisition_engine(
            home.path().join(".tokenx-test-cache"),
            home.path().to_path_buf(),
            ClientUniverse::new([ClientId::Claude]).unwrap(),
            tokenx_engine::DateRange::none(),
            tokenx_engine::scanner::ScannerSettings::default(),
            tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
            crate::acquisition::test_pricing_snapshot(),
        )
        .unwrap();
        let prepared = acquisition.prepare().unwrap();
        let generation = build_generation(&acquisition, prepared).unwrap();
        (home, generation)
    }

    fn make_app() -> App {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        App::new_for_test_with_settings(config, test_settings()).unwrap()
    }

    fn make_app_with_subscription() -> App {
        let mut settings = test_settings();
        settings.subscription.enabled = true;
        make_app_with_settings(settings)
    }

    fn make_app_with_subscription_providers(providers: &[&str]) -> App {
        let mut settings = test_settings();
        settings.subscription.enabled = true;
        settings.subscription.providers = providers
            .iter()
            .map(|provider| {
                serde_json::from_value(serde_json::Value::String((*provider).to_string())).unwrap()
            })
            .collect();
        make_app_with_settings(settings)
    }

    fn make_app_with_settings(settings: Settings) -> App {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        App::new_for_test_with_settings(config, settings).unwrap()
    }

    #[test]
    fn test_app_no_filter_default_uses_catalog() {
        let app = make_app();
        let actual = app.selected_clients().collect::<HashSet<_>>();
        let expected: HashSet<ClientId> = ClientId::iter().collect();
        assert_eq!(
            actual, expected,
            "no-filter TUI must select exactly the accepted client catalog"
        );
        assert!(actual.contains(&ClientId::Claude));
        assert_eq!(app.client_universe().as_hash_set(), expected);
    }

    #[test]
    fn explicit_clients_define_an_immutable_client_universe() {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: ClientUniverse::new([ClientId::Claude, ClientId::Codex]).unwrap(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let app = App::new_for_test_with_settings(config, test_settings()).unwrap();
        let expected = HashSet::from([ClientId::Claude, ClientId::Codex]);

        assert_eq!(
            app.client_universe(),
            ClientUniverse::new([ClientId::Claude, ClientId::Codex]).unwrap()
        );
        assert_eq!(app.selected_clients().collect::<HashSet<_>>(), expected);
    }

    #[test]
    fn installed_generation_has_one_input_footprint_for_overview_and_sessions() {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: ClientUniverse::new([ClientId::Claude, ClientId::Codex]).unwrap(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let mut app = App::new_for_test_with_settings(config, test_settings()).unwrap();
        let footprint = tokenx_engine::InputFootprint::from_client_bytes([
            (ClientId::Claude, 13),
            (ClientId::Codex, 8),
        ])
        .unwrap();

        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            footprint,
        );

        let session_total = app
            .session_snapshot()
            .client_summaries()
            .iter()
            .map(|summary| summary.space_bytes)
            .sum::<u64>();
        assert_eq!(app.total_input_bytes(), 21);
        assert_eq!(app.total_input_bytes(), session_total);
    }

    fn make_app_with_models(n: usize) -> App {
        let mut app = make_app();
        app.usage_mut_for_test().models = (0..n)
            .map(|i| UsageModelEntry {
                model_id: format!("model{}", i).into(),
                display_name: format!("model{}", i).into(),
                provider: "provider".into(),
                clients: vec![ClientId::OpenCode],
                tokens: UsageTokenBreakdown::default(),
                cost: 0.0,
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            })
            .collect();
        app
    }

    fn model_detail_accumulator() -> tokenx_engine::FrozenUsageIndex {
        let messages = [
            (
                tokenx_engine::ClientId::Claude,
                "shared-model",
                "anthropic",
                "claude-anthropic",
                11,
            ),
            (
                tokenx_engine::ClientId::Claude,
                "shared-model",
                "openrouter",
                "claude-openrouter",
                22,
            ),
            (
                tokenx_engine::ClientId::Codex,
                "shared-model",
                "openai",
                "codex-openai",
                33,
            ),
            (
                tokenx_engine::ClientId::Claude,
                "other-model",
                "anthropic",
                "claude-other",
                7,
            ),
        ]
        .map(|(client, model, provider, session, input)| {
            tokenx_engine::AttributedUsageRecord::new(
                client,
                model,
                provider,
                session,
                1_800_000_000,
                tokenx_engine::TokenBreakdown {
                    input,
                    ..Default::default()
                },
                input as f64 / 100.0,
            )
        });
        tokenx_engine::build_usage_index(
            &messages,
            tokenx_engine::DateRange::none(),
            tokenx_engine::CalendarContext::explicit("UTC").unwrap(),
        )
        .unwrap()
    }

    fn make_app_with_model_projection(group_by: tokenx_engine::GroupBy) -> App {
        let accumulator = model_detail_accumulator();
        let mut app = make_app();
        app.current_tab = Tab::Models;
        app.set_group_by_for_test(group_by);
        app.install_generation_fixture(accumulator, Vec::new(), Default::default());
        app
    }

    fn daily_usage(date: &str, cost: f64, models: Vec<(&str, &str, f64)>) -> DailyUsage {
        daily_usage_by_client(date, cost, vec![("claude", models)])
    }

    fn daily_usage_by_client(date: &str, cost: f64, clients: ClientModelCosts<'_>) -> DailyUsage {
        let mut client_breakdown = BTreeMap::new();
        let mut total_tokens = UsageTokenBreakdown::default();
        let mut total_cost = 0.0;

        for (client, models) in clients {
            let mut model_breakdown = Vec::new();
            let mut client_tokens = UsageTokenBreakdown::default();
            let mut client_cost = 0.0;

            for (model, provider, model_cost) in models {
                let tokens = UsageTokenBreakdown {
                    input: (model_cost * 100.0) as u64,
                    output: 10,
                    cache_read: 5,
                    cache_write: 0,
                    reasoning: 0,
                };
                client_tokens.input = client_tokens.input.saturating_add(tokens.input);
                client_tokens.output = client_tokens.output.saturating_add(tokens.output);
                client_tokens.cache_read =
                    client_tokens.cache_read.saturating_add(tokens.cache_read);
                total_tokens.input = total_tokens.input.saturating_add(tokens.input);
                total_tokens.output = total_tokens.output.saturating_add(tokens.output);
                total_tokens.cache_read = total_tokens.cache_read.saturating_add(tokens.cache_read);
                client_cost += model_cost;
                total_cost += model_cost;

                model_breakdown.push(DailyModelInfo {
                    provider: provider.into(),
                    model_id: model.into(),
                    display_name: model.into(),
                    workspace_key: None,
                    workspace_label: None,
                    tokens,
                    cost: model_cost,
                    messages: 1,
                });
            }

            client_breakdown.insert(
                ClientId::from_str(client).expect("test client must be accepted"),
                DailyClientInfo {
                    tokens: client_tokens,
                    cost: client_cost,
                    models: model_breakdown,
                },
            );
        }

        DailyUsage {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            tokens: total_tokens,
            cost: if cost > 0.0 { cost } else { total_cost },
            client_breakdown,
            message_count: 1,
            turn_count: 1,
        }
    }

    fn usage_data_with_graph_for_today(
        graph_today: NaiveDate,
        activity_date: NaiveDate,
    ) -> UsageProjection {
        let daily = vec![daily_usage(
            &activity_date.format("%Y-%m-%d").to_string(),
            1.0,
            vec![("gpt-5.4", "openai", 1.0)],
        )];
        let graph = tokenx_engine::build_contribution_graph_for_today(&daily, graph_today).unwrap();
        UsageProjection {
            daily,
            graph,
            ..Default::default()
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
        let outcome = app.handle_key_event(key(KeyCode::Char('q')));
        assert_eq!(outcome, KeyEventOutcome::Exit(TuiExit::Quit));
    }

    #[test]
    fn test_handle_key_quit_ctrl_c() {
        let mut app = make_app();
        let outcome = app.handle_key_event(key_with_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(outcome, KeyEventOutcome::Exit(TuiExit::Interrupted));
    }

    #[test]
    fn test_dialog_ctrl_c_still_global_quit() {
        let mut app = make_app();
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        app.open_client_picker();

        let outcome = app.handle_key_event(key_with_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(outcome, KeyEventOutcome::Exit(TuiExit::Interrupted));
        assert!(app.dialog_stack.is_active());
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
        assert_eq!(app.current_tab, Tab::Sessions);

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Overview);
    }

    #[test]
    fn test_handle_key_backtab_switch() {
        let mut app = make_app();
        assert_eq!(app.current_tab, Tab::Overview);

        app.handle_key_event(key(KeyCode::BackTab));
        assert_eq!(app.current_tab, Tab::Sessions);

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
    fn test_handle_key_tab_switch_with_subscription_enabled_includes_subscription() {
        let mut app = make_app_with_subscription();
        assert_eq!(app.current_tab, Tab::Overview);

        for expected in [
            Tab::Subscription,
            Tab::Models,
            Tab::Monthly,
            Tab::Weekly,
            Tab::Daily,
            Tab::Hourly,
            Tab::Stats,
            Tab::Agents,
            Tab::Sessions,
            Tab::Overview,
        ] {
            app.handle_key_event(key(KeyCode::Tab));
            assert_eq!(app.current_tab, expected);
        }
    }

    #[test]
    fn cli_no_refresh_overrides_enabled_setting_for_this_run() {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: true,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: None,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let settings = Settings {
            auto_refresh_enabled: true,
            ..Settings::default()
        };

        let app = App::new_for_test_with_settings(config, settings).unwrap();

        assert!(!app.auto_refresh_enabled());
        assert!(app.settings.auto_refresh_enabled);
    }

    #[test]
    fn test_initial_subscription_tab_fails_when_flag_off() {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: Some(Tab::Subscription),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };
        let result = App::new_for_test_with_settings(config, Settings::default());
        let error = match result {
            Ok(_) => panic!("a disabled explicit tab must not silently fall back"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("disabled in settings.json"));
    }

    #[test]
    fn test_get_sorted_agents_by_cost_desc() {
        let mut app = make_app();
        app.usage_mut_for_test().agents = vec![
            AgentEntry {
                agent: "builder".into(),
                client: ClientId::OpenCode,
                tokens: UsageTokenBreakdown {
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
            AgentEntry {
                agent: "reviewer".into(),
                client: ClientId::RooCode,
                tokens: UsageTokenBreakdown {
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
        assert_eq!(agents[0].agent.as_ref(), "reviewer");
        assert_eq!(agents[1].agent.as_ref(), "builder");
    }

    #[test]
    fn test_get_sorted_agents_by_tokens_asc() {
        let mut app = make_app();
        app.sort_field = SortField::Tokens;
        app.sort_direction = SortDirection::Ascending;
        app.usage_mut_for_test().agents = vec![
            AgentEntry {
                agent: "builder".into(),
                client: ClientId::OpenCode,
                tokens: UsageTokenBreakdown {
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
            AgentEntry {
                agent: "reviewer".into(),
                client: ClientId::RooCode,
                tokens: UsageTokenBreakdown {
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
        assert_eq!(agents[0].agent.as_ref(), "reviewer");
        assert_eq!(agents[1].agent.as_ref(), "builder");
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
    fn test_handle_key_left_right_switch_with_subscription_enabled() {
        let mut app = make_app_with_subscription();
        app.handle_key_event(key(KeyCode::Right));
        assert_eq!(app.current_tab, Tab::Subscription);

        app.handle_key_event(key(KeyCode::Right));
        assert_eq!(app.current_tab, Tab::Models);

        app.handle_key_event(key(KeyCode::Left));
        assert_eq!(app.current_tab, Tab::Subscription);
    }

    #[test]
    fn test_handle_key_tab_restores_target_tab_selection() {
        let mut app = make_app_with_models(5);
        app.switch_tab(Tab::Models);
        app.max_visible_items = 3;
        app.selected_index = 3;
        app.scroll_offset = 1;
        app.switch_tab(Tab::Overview);
        app.selected_graph_cell = Some((2, 4));

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.current_tab, Tab::Models);
        assert_eq!(app.selected_index, 3);
        assert_eq!(app.scroll_offset, 1);
        assert_eq!(app.selected_graph_cell, None);
    }

    #[test]
    fn test_switch_tab_preserves_each_tab_list_interaction() {
        let mut app = make_app_with_models(5);
        app.usage_mut_for_test().daily = vec![
            daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
            daily_usage("2026-05-17", 7.0, vec![("target", "openai", 7.0)]),
            daily_usage("2026-05-18", 3.0, vec![("other", "google", 3.0)]),
        ];

        app.switch_tab(Tab::Models);
        app.max_visible_items = 3;
        app.selected_index = 4;
        app.scroll_offset = 2;

        app.switch_tab(Tab::Daily);
        app.max_visible_items = 2;
        app.selected_index = 1;
        app.scroll_offset = 1;

        app.switch_tab(Tab::Models);
        assert_eq!(app.selected_index, 4);
        assert_eq!(app.scroll_offset, 2);

        app.switch_tab(Tab::Daily);
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.scroll_offset, 1);
    }

    #[test]
    fn test_switch_tab_from_daily_detail_restores_daily_parent_state() {
        let mut app = make_app_with_models(5);
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.usage_mut_for_test().daily = vec![
            daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
            daily_usage("2026-05-17", 7.0, vec![("target", "openai", 7.0)]),
            daily_usage("2026-05-18", 3.0, vec![("other", "google", 3.0)]),
        ];

        app.max_visible_items = 2;
        app.selected_index = 1;
        app.scroll_offset = 1;
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_daily_detail_active());

        app.switch_tab(Tab::Models);
        assert!(!app.is_daily_detail_active());

        app.switch_tab(Tab::Daily);
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.scroll_offset, 1);
    }

    #[test]
    fn test_handle_key_tab_resets_selection_when_target_has_no_saved_state() {
        let mut app = make_app_with_models(5);
        app.selected_index = 3;
        app.scroll_offset = 1;

        app.handle_key_event(key(KeyCode::Tab));
        assert_eq!(app.selected_index, 0);
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_enter_on_daily_opens_selected_day_detail_rows() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.usage_mut_for_test().daily = vec![
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

        assert_eq!(app.get_current_list_len().unwrap(), 2);
    }

    #[test]
    fn test_enter_on_daily_detail_uses_token_sort_default() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.usage_mut_for_test().daily = vec![daily_usage(
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
        assert_eq!(app.daily_detail_rows()[0].model.as_ref(), "z-high-token");
    }

    #[test]
    fn daily_detail_reuses_materialized_rows_and_sort_order() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.usage_mut_for_test().daily = vec![daily_usage(
            "2026-05-17",
            8.0,
            vec![
                ("a-low-token", "anthropic", 1.0),
                ("z-high-token", "openai", 7.0),
            ],
        )];

        app.handle_key_event(key(KeyCode::Enter));
        let first = app.daily_detail_rows();
        let first_rows = first.rows.as_ptr();
        let first_order = Arc::clone(&first.order);
        let repeated = app.daily_detail_rows();

        assert_eq!(repeated.rows.as_ptr(), first_rows);
        assert!(Arc::ptr_eq(&first_order, &repeated.order));

        app.handle_key_event(key(KeyCode::Esc));
        app.handle_key_event(key(KeyCode::Enter));
        let reopened = app.daily_detail_rows();
        assert_eq!(reopened.rows.as_ptr(), first_rows);
    }

    #[test]
    fn test_esc_from_daily_detail_restores_daily_selection() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.usage_mut_for_test().daily = vec![
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
        app.max_visible_items = 5;

        app.handle_key_event(key(KeyCode::Esc));

        assert_eq!(app.current_tab, Tab::Daily);
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.scroll_offset, 1);
        assert_eq!(app.max_visible_items, 2);
        assert_eq!(app.stored_list_interaction(Tab::Daily).visible, 2);
        assert_eq!(app.get_current_list_len().unwrap(), 3);
    }

    #[test]
    fn test_close_daily_detail_reanchors_selection_by_date_after_sort_change() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.usage_mut_for_test().daily = vec![
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
        app.usage_mut_for_test().daily = vec![
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

        let refreshed = UsageProjection {
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
        assert!(app.daily_detail_rows().is_empty());
    }

    #[test]
    fn test_update_data_keeps_daily_detail_when_date_still_present() {
        let mut app = make_app();
        app.current_tab = Tab::Daily;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.usage_mut_for_test().daily = vec![
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

        let refreshed = UsageProjection {
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
        app.set_group_by_for_test(tokenx_engine::GroupBy::ClientModel);
        app.usage_mut_for_test().daily = vec![daily_usage_by_client(
            "2026-05-17",
            0.0,
            vec![
                ("claude", vec![("claude:gpt-5", "openai", 5.0)]),
                ("codex", vec![("codex:gpt-5", "openai", 2.0)]),
            ],
        )];

        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_daily_detail_active());
        assert_eq!(app.daily_detail_rows().len(), 2);

        app.set_group_by_for_test(tokenx_engine::GroupBy::Model);
        app.update_data(UsageProjection {
            daily: vec![daily_usage_by_client(
                "2026-05-17",
                0.0,
                vec![
                    ("claude", vec![("gpt-5", "openai", 5.0)]),
                    ("codex", vec![("gpt-5", "openai", 2.0)]),
                ],
            )],
            ..Default::default()
        });

        let rows = app.daily_detail_rows();
        assert!(app.is_daily_detail_active());
        assert_eq!(
            app.daily_detail_date(),
            Some(NaiveDate::from_ymd_opt(2026, 5, 17).unwrap())
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].clients.as_ref(),
            &[ClientId::Claude, ClientId::Codex]
        );
        assert_eq!(rows[0].model.as_ref(), "gpt-5");
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
        app.usage_mut_for_test().daily = vec![
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
        assert_eq!(app.get_current_list_len().unwrap(), 2);
        assert_eq!(app.period_detail_rows()[0].model.as_ref(), "target-a");
        assert_eq!(
            app.selected_period_detail.unwrap().start_date,
            selected_period
        );
    }

    #[test]
    fn test_period_detail_model_name_falls_back_to_model_key() {
        let mut app = make_app();
        app.current_tab = Tab::Monthly;
        app.usage_mut_for_test().daily = vec![daily_usage(
            "2026-05-17",
            7.0,
            vec![("fallback-model", "openai", 7.0)],
        )];
        app.usage_mut_for_test().daily[0]
            .client_breakdown
            .get_mut(&ClientId::Claude)
            .unwrap()
            .models
            .iter_mut()
            .find(|model| model.model_id.as_ref() == "fallback-model")
            .unwrap()
            .display_name = "".into();

        app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(app.period_detail_rows()[0].model.as_ref(), "fallback-model");
    }

    #[test]
    fn test_esc_from_weekly_detail_restores_week_selection() {
        let mut app = make_app();
        app.current_tab = Tab::Weekly;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.usage_mut_for_test().daily = vec![
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
        app.max_visible_items = 5;

        app.handle_key_event(key(KeyCode::Esc));

        assert!(!app.is_period_detail_active());
        assert_eq!(app.current_tab, Tab::Weekly);
        assert_eq!(app.sort_field, SortField::Date);
        assert_eq!(app.sort_direction, SortDirection::Descending);
        assert_eq!(app.selected_index, 2);
        assert_eq!(app.scroll_offset, 1);
        assert_eq!(app.max_visible_items, 2);
        assert_eq!(app.stored_list_interaction(Tab::Weekly).visible, 2);
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
        app.usage_mut_for_test().daily = vec![
            daily_usage("2026-06-01", 1.0, vec![("target", "openai", 1.0)]),
            daily_usage("2026-06-10", 3.0, vec![("other", "google", 3.0)]),
        ];

        app.selected_index = 1;
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_period_detail_active_for_kind(PeriodKind::Weekly));

        let refreshed = UsageProjection {
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
        assert!(app.period_detail_rows().is_empty());
    }

    #[test]
    fn test_period_detail_uses_grouped_detail_rows() {
        let mut app = make_app();
        app.current_tab = Tab::Monthly;
        app.sort_field = SortField::Date;
        app.sort_direction = SortDirection::Descending;
        app.set_group_by_for_test(tokenx_engine::GroupBy::Model);
        app.usage_mut_for_test().daily = vec![
            daily_usage_by_client(
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

        let rows = app.period_detail_rows();
        assert!(app.is_period_detail_active_for_kind(PeriodKind::Monthly));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].clients.as_ref(),
            &[ClientId::Claude, ClientId::Codex]
        );
        assert_eq!(rows[0].model.as_ref(), "gpt-5");
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
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: Some(Tab::Models),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };

        let app = App::new_for_test_with_settings(config, Settings::default()).unwrap();

        assert_eq!(app.current_tab, Tab::Models);
        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
    }

    #[test]
    fn test_models_default_sort_shows_highest_tokens_first() {
        let mut app = make_app();
        app.usage_mut_for_test().models = vec![
            UsageModelEntry {
                model_id: "expensive-low-token".into(),
                display_name: "expensive-low-token".into(),
                provider: "anthropic".into(),
                clients: vec![ClientId::Claude],
                tokens: UsageTokenBreakdown {
                    input: 10,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: 100.0,
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
            UsageModelEntry {
                model_id: "cheap-high-token".into(),
                display_name: "cheap-high-token".into(),
                provider: "anthropic".into(),
                clients: vec![ClientId::Claude],
                tokens: UsageTokenBreakdown {
                    input: 1_000,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: 1.0,
                session_count: 1,
                workspace_key: None,
                workspace_label: None,
            },
        ];

        app.switch_tab(Tab::Models);

        assert_eq!(app.sort_field, SortField::Tokens);
        assert_eq!(app.sort_direction, SortDirection::Descending);
        assert_eq!(
            app.get_sorted_models()[0].model_id.as_ref(),
            "cheap-high-token"
        );
    }

    #[test]
    fn test_initial_hourly_tab_uses_hourly_sort_default() {
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: Some(Tab::Hourly),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };

        let app = App::new_for_test(config).unwrap();

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
        app.usage_mut_for_test().daily = vec![
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
    fn subscription_tab_key_scrolls_text_viewport_without_table_selection() {
        let mut app = make_app_with_subscription();
        app.current_tab = Tab::Subscription;
        app.selected_index = 3;
        app.scroll_offset = 2;
        app.set_subscription_text_viewport(4, 10);

        app.handle_key_event(key(KeyCode::Down));

        assert_eq!(app.subscription_viewport.scroll, 1);
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
    fn subscription_tab_mouse_wheel_scrolls_text_viewport_without_table_selection() {
        let mut app = make_app_with_subscription();
        app.current_tab = Tab::Subscription;
        app.selected_index = 4;
        app.scroll_offset = 3;
        app.set_subscription_text_viewport(4, 10);

        app.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(app.subscription_viewport.scroll, 1);
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
        app.usage_mut_for_test().models.clear();
        app.selected_index = 0;
        app.move_selection_up();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn test_move_selection_down_empty_list_noop() {
        let mut app = make_app();
        app.usage_mut_for_test().models.clear();
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
        let settings_path = app.product_paths.settings_file();

        app.handle_key_event(key(KeyCode::Char('p')));
        assert_ne!(app.theme.name, initial_theme);
        assert!(
            settings_path.exists(),
            "theme save must use the isolated test settings file"
        );

        for _ in 1..ThemeName::all().len() {
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
        app.handle_key_event(key(KeyCode::Char('r')));
        assert_eq!(app.take_refresh_requests(), vec![RefreshRequest::Manual]);
    }

    #[test]
    fn background_loading_tracks_one_elapsed_interval_per_run() {
        let mut app = make_app();
        assert!(app.background_load_elapsed().is_none());

        app.set_refresh_loading_for_test(true);
        let first_elapsed = app.background_load_elapsed().unwrap();
        assert!(app.background_load_elapsed().is_some());

        app.set_refresh_loading_for_test(true);
        assert!(app.background_load_elapsed().unwrap() >= first_elapsed);

        app.set_refresh_loading_for_test(false);
        assert!(app.background_load_elapsed().is_none());
    }

    #[test]
    fn test_handle_key_refresh_while_loading_does_not_queue_reload() {
        let mut app = make_app();
        app.set_refresh_loading_for_test(true);

        app.handle_key_event(key(KeyCode::Char('r')));

        assert_eq!(app.take_refresh_requests(), vec![RefreshRequest::Manual]);
        assert_eq!(app.status_message.as_deref(), Some("Refresh queued"));
    }

    #[test]
    fn model_group_enter_splits_one_model_by_client_and_provider() {
        let mut app = make_app_with_model_projection(tokenx_engine::GroupBy::Model);
        app.selected_index = app
            .get_sorted_models()
            .iter()
            .position(|model| model.model_id.as_ref() == "shared-model")
            .unwrap();

        app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(
            app.selected_model_detail,
            Some(ModelDetailSelection {
                model: "shared-model".to_string(),
                client: None,
            })
        );
        let mut rows = app
            .get_sorted_models()
            .into_iter()
            .map(|model| (model.clients.clone(), model.provider.to_string()))
            .collect::<Vec<_>>();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                (vec![ClientId::Claude], "anthropic".to_string()),
                (vec![ClientId::Claude], "openrouter".to_string()),
                (vec![ClientId::Codex], "openai".to_string()),
            ]
        );
    }

    #[test]
    fn client_model_group_enter_keeps_outer_client_and_splits_providers() {
        let mut app = make_app_with_model_projection(tokenx_engine::GroupBy::ClientModel);
        app.selected_index = app
            .get_sorted_models()
            .iter()
            .position(|model| {
                model.model_id.as_ref() == "shared-model"
                    && model.clients.as_slice() == [ClientId::Claude]
            })
            .unwrap();

        app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(
            app.selected_model_detail,
            Some(ModelDetailSelection {
                model: "shared-model".to_string(),
                client: Some(ClientId::Claude),
            })
        );
        let mut providers = app
            .get_sorted_models()
            .into_iter()
            .map(|model| {
                assert_eq!(model.clients, [ClientId::Claude]);
                model.provider.to_string()
            })
            .collect::<Vec<_>>();
        providers.sort();
        assert_eq!(providers, vec!["anthropic", "openrouter"]);
    }

    #[test]
    fn model_detail_escape_restores_outer_state_without_reprojection() {
        let mut app = make_app_with_model_projection(tokenx_engine::GroupBy::Model);
        app.sort_field = SortField::Cost;
        app.sort_direction = SortDirection::Ascending;
        app.selected_index = app
            .get_sorted_models()
            .iter()
            .position(|model| model.model_id.as_ref() == "shared-model")
            .unwrap();
        let outer_selection = app.selected_index;

        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_model_detail_active());
        assert!(app.model_detail_models.is_some());
        app.set_sort(SortField::Cost);

        app.handle_key_event(key(KeyCode::Esc));

        assert!(!app.is_model_detail_active());
        assert!(app.model_detail_models.is_some());
        assert_eq!(app.selected_index, outer_selection);
        assert_eq!(app.sort_field, SortField::Cost);
        assert_eq!(app.sort_direction, SortDirection::Ascending);

        app.handle_key_event(key(KeyCode::Enter));
        assert!(
            app.is_model_detail_active(),
            "re-enter should reuse the cached provider projection"
        );
    }

    #[test]
    fn model_detail_is_disabled_for_already_expanded_groupings() {
        for group_by in [
            tokenx_engine::GroupBy::ClientProviderModel,
            tokenx_engine::GroupBy::WorkspaceModel,
        ] {
            let mut app = make_app_with_model_projection(group_by);
            app.handle_key_event(key(KeyCode::Enter));

            assert!(!app.is_model_detail_active());
            assert!(app.model_detail_models.is_none());
        }
    }

    #[test]
    fn refresh_invalidates_model_detail_but_client_projection_refreshes_it() {
        let mut app = make_app_with_model_projection(tokenx_engine::GroupBy::Model);
        app.selected_index = app
            .get_sorted_models()
            .iter()
            .position(|model| model.model_id.as_ref() == "shared-model")
            .unwrap();
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.model_detail_models.is_some());

        let data = app.usage().clone();
        app.update_data(data);
        assert!(!app.is_model_detail_active());
        assert!(app.model_detail_models.is_none());

        app.selected_index = app
            .get_sorted_models()
            .iter()
            .position(|model| model.model_id.as_ref() == "shared-model")
            .unwrap();
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.model_detail_models.is_some());
        app.apply_ui_command(UiCommand::ProjectClients(HashSet::from([ClientId::Claude])));

        assert!(app.is_model_detail_active());
        assert_eq!(
            app.selected_model_detail,
            Some(ModelDetailSelection {
                model: "shared-model".to_string(),
                client: None,
            })
        );
        let rows = app.get_sorted_models();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|model| model.clients.as_slice() == [ClientId::Claude]));
        assert!(app.model_detail_models.is_some());
        assert_eq!(
            app.selected_clients().collect::<HashSet<_>>(),
            HashSet::from([ClientId::Claude])
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Clients filtered locally; model details updated")
        );

        app.handle_key_event(key(KeyCode::Esc));

        assert!(!app.is_model_detail_active());
        assert_eq!(
            app.get_sorted_models()[app.selected_index]
                .model_id
                .as_ref(),
            "shared-model"
        );
    }

    #[test]
    fn client_projection_exits_model_detail_when_locked_selection_disappears() {
        let mut app = make_app_with_model_projection(tokenx_engine::GroupBy::ClientModel);
        app.selected_index = app
            .get_sorted_models()
            .iter()
            .position(|model| {
                model.model_id.as_ref() == "shared-model"
                    && model.clients.as_slice() == [ClientId::Codex]
            })
            .unwrap();
        app.handle_key_event(key(KeyCode::Enter));
        assert!(app.is_model_detail_active());
        assert!(app.model_detail_models.is_some());
        app.apply_ui_command(UiCommand::ProjectClients(HashSet::from([ClientId::Claude])));

        assert!(!app.is_model_detail_active());
        assert!(app.model_detail_models.is_none());
        assert_eq!(
            app.selected_clients().collect::<HashSet<_>>(),
            HashSet::from([ClientId::Claude])
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Selected model is not available for the current Client filter")
        );
    }

    #[test]
    #[serial]
    fn group_by_change_reprojects_the_installed_generation() {
        let (_home, generation) = load_test_generation();
        let mut app = make_app();
        let universe = generation.universe().clone();
        let clients = universe.as_hash_set();
        app.set_selected_clients_for_test(clients);
        app.current_tab = Tab::Models;
        app.set_group_by_for_test(tokenx_engine::GroupBy::ClientModel);
        app.install_generation(generation).unwrap();
        let health_complete = app.generation_health().unwrap().complete();
        let failed_inputs = app.generation_health().unwrap().failed_inputs();
        app.set_refresh_loading_for_test(true);
        app.fail_refresh_for_test("retained error".to_string());
        app.set_generation_cache_warning(Some("retained cache warning".to_string()));
        let refresh_status = app.refresh_status();

        app.handle_key_event(key(KeyCode::Char('g')));
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Down));
        app.handle_key_event(key(KeyCode::Enter));

        assert_eq!(app.group_by(), tokenx_engine::GroupBy::WorkspaceModel);
        assert_eq!(app.usage().models.len(), 2);
        assert!(app
            .usage()
            .models
            .iter()
            .all(|model| model.workspace_key.is_some()));
        assert!(app.take_refresh_requests().is_empty());
        assert!(!app.is_background_loading());
        assert_eq!(app.refresh_status(), refresh_status);
        assert_eq!(app.generation_health().unwrap().complete(), health_complete);
        assert_eq!(
            app.generation_health().unwrap().failed_inputs(),
            failed_inputs
        );
        assert!(matches!(
            app.local_usage_status(),
            LocalUsageStatus::Degraded {
                diagnostic: "retained error"
            }
        ));
        assert_eq!(
            app.generation_cache_warning(),
            Some("retained cache warning")
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Regrouped by workspace,model")
        );

        let export = crate::report::build_usage_report_json(
            app.usage(),
            app.generation_health().unwrap(),
            &app.export_group_by(),
        )
        .unwrap();
        let exported: serde_json::Value = serde_json::from_str(&export).unwrap();
        assert_eq!(exported["groupBy"], "workspace,model");
        assert!(exported["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| model.get("workspaceKey").is_some()));
    }

    #[test]
    fn overview_summary_tracks_local_projection_without_digest_invalidation() {
        let mut app = make_app();
        app.update_data(UsageProjection {
            daily: vec![daily_usage(
                "2026-07-20",
                1.0,
                vec![("gpt-5.5", "openai", 1.0)],
            )],
            ..UsageProjection::default()
        });

        assert_eq!(app.overview_summary().active_days, 1);
        assert_eq!(app.overview_summary().model_count, 1);
        assert_eq!(
            app.overview_summary()
                .favorite_model
                .as_ref()
                .map(|favorite| favorite.id.as_str()),
            Some("gpt-5.5")
        );

        app.update_projected_data(UsageProjection {
            daily: vec![
                daily_usage("2026-07-20", 2.0, vec![("qwen3-coder-plus", "qwen", 2.0)]),
                daily_usage("2026-07-21", 3.0, vec![("kimi-k2", "kimi", 3.0)]),
            ],
            ..UsageProjection::default()
        });

        assert_eq!(app.overview_summary().active_days, 2);
        assert_eq!(app.overview_summary().model_count, 2);
        assert_eq!(
            app.overview_summary()
                .favorite_model
                .as_ref()
                .map(|favorite| favorite.id.as_str()),
            Some("kimi-k2")
        );
    }

    #[test]
    fn test_group_by_is_unavailable_without_an_installed_generation() {
        let mut app = make_app();
        app.current_tab = Tab::Models;
        app.set_group_by_for_test(tokenx_engine::GroupBy::ClientModel);

        app.handle_key_event(key(KeyCode::Char('g')));

        assert_eq!(app.group_by(), tokenx_engine::GroupBy::ClientModel);
        assert!(!app.dialog_stack.is_active());
        assert!(app.take_refresh_requests().is_empty());
        assert_eq!(
            app.status_message.as_deref(),
            Some("Group By is unavailable until local data finishes loading")
        );
        assert_eq!(app.status_message_tone(), StatusTone::Warning);
    }

    #[test]
    fn test_group_by_is_unavailable_during_cold_load() {
        let mut app = make_app();
        app.current_tab = Tab::Models;
        app.set_refresh_loading_for_test(true);
        app.set_group_by_for_test(tokenx_engine::GroupBy::ClientModel);

        app.handle_key_event(key(KeyCode::Char('g')));

        assert_eq!(app.group_by(), tokenx_engine::GroupBy::ClientModel);
        assert!(!app.dialog_stack.is_active());
        assert!(app.take_refresh_requests().is_empty());
        assert_eq!(
            app.status_message.as_deref(),
            Some("Group By is unavailable until local data finishes loading")
        );
        assert_eq!(app.status_message_tone(), StatusTone::Warning);
    }

    #[test]
    fn export_group_by_matches_the_installed_projection() {
        let app = make_app_with_model_projection(tokenx_engine::GroupBy::WorkspaceModel);
        assert_eq!(
            app.export_group_by(),
            tokenx_engine::GroupBy::WorkspaceModel
        );
    }

    #[test]
    fn test_client_picker_reprojects_without_requesting_reload() {
        let mut app = make_app();
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        let refresh_status = app.refresh_status();

        app.handle_key_event(key(KeyCode::Char('s')));
        app.handle_key_event(key(KeyCode::Char(' ')));
        app.handle_key_event(key(KeyCode::Enter));

        assert!(app.take_refresh_requests().is_empty());
        assert_eq!(app.refresh_status(), refresh_status);
    }

    #[test]
    fn invalid_projection_command_leaves_the_installed_state_unchanged() {
        let mut app = make_app();
        app.set_selected_clients_for_test(HashSet::from([ClientId::Claude]));
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        let original_clients = app.selected_clients().collect::<HashSet<_>>();
        let original_group_by = app.group_by();

        app.apply_ui_command(UiCommand::ProjectClients(HashSet::from([ClientId::Codex])));

        assert_eq!(
            app.selected_clients().collect::<HashSet<_>>(),
            original_clients
        );
        assert_eq!(app.group_by(), original_group_by);
        assert_eq!(
            app.status_message.as_deref(),
            Some("Client selection is outside the loaded client universe")
        );
        assert!(app.take_refresh_requests().is_empty());
    }

    #[test]
    fn test_g_opens_group_picker_only_on_group_keyed_tabs() {
        for tab in [Tab::Models, Tab::Daily, Tab::Monthly, Tab::Weekly] {
            let mut app = make_app();
            app.current_tab = tab;
            app.install_generation_fixture(
                tokenx_engine::FrozenUsageIndex::new(),
                Vec::new(),
                Default::default(),
            );

            app.handle_key_event(key(KeyCode::Char('g')));

            assert!(
                app.dialog_stack.is_active(),
                "g should open the Group By picker on {tab:?}"
            );
        }

        for tab in [
            Tab::Overview,
            Tab::Subscription,
            Tab::Hourly,
            Tab::Stats,
            Tab::Agents,
            Tab::Sessions,
        ] {
            let mut app = make_app();
            app.current_tab = tab;

            app.handle_key_event(key(KeyCode::Char('g')));

            assert!(
                !app.dialog_stack.is_active(),
                "g should be a no-op on {tab:?}"
            );
        }
    }

    #[test]
    fn test_client_picker_waits_until_close_before_local_projection() {
        let mut app = make_app();
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            Default::default(),
        );
        let original_clients = app.selected_clients().collect::<HashSet<_>>();

        app.handle_key_event(key(KeyCode::Char('s')));
        assert!(app.dialog_stack.is_active());

        app.handle_key_event(key(KeyCode::Char(' ')));
        assert!(app.dialog_stack.is_active());
        assert!(app.take_refresh_requests().is_empty());
        assert_eq!(
            app.selected_clients().collect::<HashSet<_>>(),
            original_clients
        );

        app.on_tick();
        assert!(app.take_refresh_requests().is_empty());
        assert_eq!(
            app.selected_clients().collect::<HashSet<_>>(),
            original_clients
        );

        app.handle_key_event(key(KeyCode::Enter));

        assert!(!app.dialog_stack.is_active());
        assert!(app.take_refresh_requests().is_empty());
        assert_ne!(
            app.selected_clients().collect::<HashSet<_>>(),
            original_clients
        );
    }

    #[test]
    fn test_client_picker_is_unavailable_during_cold_load() {
        let mut app = make_app();
        app.set_refresh_loading_for_test(true);

        app.handle_key_event(key(KeyCode::Char('s')));

        assert!(!app.dialog_stack.is_active());
        assert!(app.take_refresh_requests().is_empty());
        assert_eq!(
            app.status_message.as_deref(),
            Some("Clients are unavailable until local data finishes loading")
        );
        assert_eq!(app.status_message_tone(), StatusTone::Warning);
    }

    // ── handle_key_event: misc keys ─────────────────────────────────

    #[test]
    fn test_entering_stats_selects_today_instead_of_latest_subscription_day() {
        let mut app = make_app();
        let today = app.effective_date();
        let activity_date = today - chrono::Duration::days(1);
        app.update_data(usage_data_with_graph_for_today(today, activity_date));
        let today_cell = app.graph_cell_for_date(today).unwrap();
        let activity_cell = app.graph_cell_for_date(activity_date).unwrap();
        assert_ne!(today_cell, activity_cell);

        app.selected_graph_cell = Some(activity_cell);
        app.switch_tab(Tab::Stats);

        assert_eq!(app.selected_graph_cell, Some(today_cell));
        assert_eq!(app.graph_date_for_cell(today_cell), Some(today));
    }

    #[test]
    fn test_initial_stats_tab_selects_today_from_cached_graph() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap();
        let activity_date = today - chrono::Duration::days(2);
        let config = TuiConfig {
            theme: Some(ThemeName::Blue),
            refresh: 0,
            no_refresh: false,
            client_universe: tokenx_engine::ClientUniverse::all(),
            initial_tab: Some(Tab::Stats),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        };

        let mut app = App::new_for_test_with_settings(config, test_settings()).unwrap();
        app.update_data(usage_data_with_graph_for_today(today, activity_date));

        assert_eq!(
            app.selected_graph_cell
                .and_then(|cell| app.graph_date_for_cell(cell)),
            Some(today)
        );
    }

    #[test]
    fn test_stats_selects_today_when_graph_arrives_after_entry() {
        let mut app = make_app();
        let today = app.effective_date();

        app.switch_tab(Tab::Stats);
        assert_eq!(app.selected_graph_cell, None);

        app.update_data(usage_data_with_graph_for_today(
            today,
            today - chrono::Duration::days(1),
        ));

        assert_eq!(
            app.selected_graph_cell
                .and_then(|cell| app.graph_date_for_cell(cell)),
            Some(today)
        );
    }

    #[test]
    fn test_stats_tab_reclick_keeps_manual_day_selection() {
        let mut app = make_app();
        let today = app.effective_date();
        let activity_date = today - chrono::Duration::days(3);
        app.update_data(usage_data_with_graph_for_today(today, activity_date));
        app.switch_tab(Tab::Stats);
        let activity_cell = app.graph_cell_for_date(activity_date).unwrap();
        app.selected_graph_cell = Some(activity_cell);

        // Clicking the already-active Stats tab header must not reset the
        // manually chosen day back to today.
        app.switch_tab(Tab::Stats);

        assert_eq!(app.selected_graph_cell, Some(activity_cell));
    }

    #[test]
    fn test_stats_escape_prevents_refresh_from_reselecting_today() {
        let mut app = make_app();
        let today = app.effective_date();
        let activity_date = today - chrono::Duration::days(1);
        app.update_data(usage_data_with_graph_for_today(today, activity_date));
        app.switch_tab(Tab::Stats);
        assert!(app.selected_graph_cell.is_some());

        app.handle_key_event(key(KeyCode::Esc));
        assert_eq!(app.selected_graph_cell, None);

        app.update_data(usage_data_with_graph_for_today(today, activity_date));
        assert_eq!(app.selected_graph_cell, None);
    }

    #[test]
    fn test_stats_selection_is_remapped_by_date_after_graph_rebuild() {
        let mut app = make_app();
        let today = app.effective_date();
        let selected_date = today - chrono::Duration::days(10);
        app.update_data(usage_data_with_graph_for_today(today, selected_date));
        app.switch_tab(Tab::Stats);
        let old_cell = app.graph_cell_for_date(selected_date).unwrap();
        app.selected_graph_cell = Some(old_cell);

        app.update_data(usage_data_with_graph_for_today(
            today + chrono::Duration::days(7),
            selected_date,
        ));

        let new_cell = app.selected_graph_cell.unwrap();
        assert_ne!(new_cell, old_cell);
        assert_eq!(app.graph_date_for_cell(new_cell), Some(selected_date));
    }

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
    fn test_handle_key_unrecognized_continues() {
        let mut app = make_app();
        let outcome = app.handle_key_event(key(KeyCode::F(12)));
        assert_eq!(outcome, KeyEventOutcome::Continue);
    }

    #[test]
    fn test_handle_key_auto_refresh_toggle() {
        let mut app = make_app();
        app.handle_key_event(key_with_mod(KeyCode::Char('R'), KeyModifiers::SHIFT));
        assert_eq!(
            app.take_refresh_controls(),
            vec![RefreshControl::ToggleAutomatic]
        );
    }

    #[test]
    fn test_subscription_fetch_completion_updates_subscription_status_and_timestamp() {
        let mut app = make_app();
        let (_tx, rx) = std::sync::mpsc::channel();

        app.start_subscription_fetch_for_test(rx);

        assert_eq!(
            app.subscription_status_message.as_deref(),
            Some("Fetching subscription...")
        );
        assert!(app.status_message.is_none());
        assert!(app.has_subscription_fetch_history());
        assert!(app.is_fetching_subscription());
        assert!(app.subscription_fetch_elapsed().is_some());
    }

    #[test]
    fn test_subscription_fetch_completion_sets_subscription_check_clock() {
        let mut app = make_app();
        let (tx, rx) = std::sync::mpsc::channel();
        app.start_subscription_fetch_for_test(rx);
        tx.send(SubscriptionBatch::default()).unwrap();

        app.on_tick();

        assert_eq!(
            app.subscription_status_message.as_deref(),
            Some("No subscription data available")
        );
        assert!(app.status_message.is_none());
        assert!(app.last_subscription_check().is_some());
        assert!(!app.is_fetching_subscription());
        assert!(app.subscription_fetch_elapsed().is_none());
    }

    fn subscription_output(provider: ProviderId) -> SubscriptionOutput {
        SubscriptionOutput {
            provider,
            stale: false,
            account: None,
            plan: None,
            email: None,
            metrics: vec![crate::subscription::UsageMetric {
                label: "Weekly".to_string(),
                used_percent: 20.0,
                remaining_percent: 80.0,
                remaining_label: None,
                resets_at: None,
            }],
        }
    }

    #[test]
    fn empty_subscription_batch_retains_installed_snapshot() {
        let mut app = make_app();
        let installed = subscription_output(ProviderId::Codex);
        app.replace_subscription_outputs_for_test(vec![installed.clone()]);

        app.install_subscription_batch(
            SubscriptionBatch {
                outputs: Vec::new(),
                errors: vec![crate::subscription::SubscriptionError {
                    provider_id: Some(ProviderId::Claude),
                    provider: "Claude".to_string(),
                    message: "credential expired".to_string(),
                }],
            },
            |_| Ok(()),
        );

        let retained = &app.subscription_outputs()[0];
        assert_eq!(retained.provider, installed.provider);
        assert!(retained.stale);
        assert_eq!(app.subscription_errors().len(), 1);
        assert_eq!(
            app.subscription_status_message.as_deref(),
            Some("Subscription data loaded with provider errors")
        );
    }

    #[test]
    fn nonempty_subscription_batch_replaces_snapshot_and_keeps_all_faults_visible() {
        let mut app = make_app();
        app.replace_subscription_outputs_for_test(vec![subscription_output(ProviderId::Claude)]);
        let replacement = subscription_output(ProviderId::Codex);

        app.install_subscription_batch(
            SubscriptionBatch {
                outputs: vec![replacement.clone()],
                errors: vec![crate::subscription::SubscriptionError {
                    provider_id: Some(ProviderId::Claude),
                    provider: "Claude".to_string(),
                    message: "credential rejected".to_string(),
                }],
            },
            |_| Err(anyhow::anyhow!("cache directory is read-only")),
        );

        assert_eq!(app.subscription_outputs().len(), 2);
        assert_eq!(app.subscription_outputs()[0].provider, ProviderId::Claude);
        assert!(app.subscription_outputs()[0].stale);
        assert_eq!(app.subscription_outputs()[1], replacement);
        assert_eq!(app.subscription_errors().len(), 2);
        assert_eq!(app.subscription_errors()[1].provider, "Subscription cache");
        assert_eq!(
            app.subscription_status_message.as_deref(),
            Some("Subscription data loaded with provider errors")
        );
    }

    #[test]
    fn test_fetch_subscription_while_fetching_reports_in_progress() {
        let mut app = make_app();
        app.current_tab = Tab::Subscription;
        let (_tx, rx) = std::sync::mpsc::channel();
        app.start_subscription_fetch_for_test(rx);
        let started_elapsed = app.subscription_fetch_elapsed().unwrap();

        app.fetch_subscription();

        assert_eq!(
            app.subscription_status_message.as_deref(),
            Some("Subscription fetch already in progress")
        );
        assert!(app.status_message.is_none());
        assert!(app.is_fetching_subscription());
        assert!(app.subscription_fetch_elapsed().unwrap() >= started_elapsed);
    }

    #[test]
    fn test_switching_to_subscription_without_enabled_providers_does_not_fetch() {
        let mut app = make_app_with_subscription();
        app.current_tab = Tab::Overview;

        app.update_data(UsageProjection::default());
        app.switch_tab(Tab::Subscription);

        assert!(!app.has_subscription_fetch_history());
        assert!(!app.is_fetching_subscription());
    }

    #[test]
    fn test_initial_subscription_fetch_requires_enabled_provider() {
        let mut app = make_app_with_subscription();
        app.current_tab = Tab::Subscription;
        assert!(!app.should_start_initial_subscription_fetch());

        let mut app = make_app_with_subscription_providers(&["codex"]);
        app.current_tab = Tab::Subscription;
        assert!(app.should_start_initial_subscription_fetch());
    }

    #[test]
    fn test_initial_subscription_fetch_starts_only_once_per_session() {
        let mut app = make_app_with_subscription_providers(&["codex"]);
        app.current_tab = Tab::Overview;
        assert!(!app.should_start_initial_subscription_fetch());

        app.current_tab = Tab::Subscription;
        assert!(app.should_start_initial_subscription_fetch());
        app.maybe_fetch_subscription_on_entry();

        app.current_tab = Tab::Models;
        assert!(!app.should_start_initial_subscription_fetch());

        app.current_tab = Tab::Subscription;
        assert!(!app.should_start_initial_subscription_fetch());
    }

    #[test]
    fn completed_manual_fetch_prevents_an_initial_fetch_on_first_tab_entry() {
        let mut app = make_app_with_subscription_providers(&["codex"]);
        assert_eq!(app.current_tab, Tab::Overview);

        app.fetch_subscription();
        let (_, sender) = app
            .take_subscription_request()
            .expect("manual request is queued");
        sender.send(SubscriptionBatch::default()).unwrap();
        app.on_tick();
        assert!(!app.is_fetching_subscription());

        app.switch_tab(Tab::Subscription);

        assert!(app.take_subscription_request().is_none());
        assert!(!app.is_fetching_subscription());
        assert!(app.has_subscription_fetch_history());
    }

    #[test]
    fn test_subscription_tab_rejects_generation_refresh_keys() {
        let mut app = make_app_with_subscription_providers(&["codex"]);
        app.current_tab = Tab::Subscription;
        let refresh_status = app.refresh_status();

        app.handle_key_event(key(KeyCode::Char('r')));
        app.handle_key_event(key_with_mod(KeyCode::Char('R'), KeyModifiers::SHIFT));
        app.handle_key_event(key(KeyCode::Char('+')));
        app.handle_key_event(key(KeyCode::Char('-')));
        app.handle_key_event(key(KeyCode::Char('e')));

        assert!(app.take_refresh_requests().is_empty());
        assert!(app.take_refresh_controls().is_empty());
        assert_eq!(app.refresh_status(), refresh_status);
        assert!(app.status_message.is_none());
        assert!(!app.has_subscription_fetch_history());
        assert!(!app.is_fetching_subscription());
    }

    #[test]
    fn test_u_outside_subscription_does_not_fetch_subscription() {
        let mut app = make_app_with_subscription_providers(&["codex"]);
        app.current_tab = Tab::Overview;

        app.handle_key_event(key(KeyCode::Char('u')));

        assert!(!app.has_subscription_fetch_history());
        assert!(!app.is_fetching_subscription());
    }

    #[test]
    fn test_u_on_subscription_without_enabled_providers_reports_disabled() {
        let mut app = make_app_with_subscription();
        app.current_tab = Tab::Subscription;

        app.handle_key_event(key(KeyCode::Char('u')));

        assert!(!app.has_subscription_fetch_history());
        assert_eq!(
            app.subscription_status_message.as_deref(),
            Some("No subscription providers enabled")
        );
        assert!(app.status_message.is_none());
    }

    #[test]
    fn test_handle_key_increase_decrease_refresh() {
        let mut app = make_app();

        app.handle_key_event(key(KeyCode::Char('+')));
        app.handle_key_event(key(KeyCode::Char('-')));
        assert_eq!(
            app.take_refresh_controls(),
            vec![
                RefreshControl::IncreaseInterval,
                RefreshControl::DecreaseInterval
            ]
        );
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
    fn tick_advances_effective_date_without_an_installed_generation() {
        let mut app = make_app();
        let next_day = NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();

        app.on_tick_for_date(next_day);

        assert_eq!(app.effective_date(), next_day);
        assert!(!app.has_installed_generation());
    }

    #[test]
    fn tick_reprojects_an_installed_generation_at_local_midnight() {
        let mut app = make_app();
        app.install_generation_fixture(
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            tokenx_engine::InputFootprint::default(),
        );
        let next_day = NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();

        app.on_tick_for_date(next_day);

        assert_eq!(app.effective_date(), next_day);
        assert!(app.has_installed_generation());
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
        app.on_tick();
        assert!(app.status_message.is_none());
        assert!(app.status_message_time.is_none());
    }

    #[test]
    fn test_on_tick_keeps_fresh_status() {
        let mut app = make_app();
        app.set_status("fresh message");

        app.on_tick();
        assert!(app.status_message.is_some());
        assert_eq!(app.status_message.as_ref().unwrap(), "fresh message");
    }

    #[test]
    fn installing_generation_restores_typed_pricing_status() {
        use tokenx_engine::pricing::PricingDiagnostic;

        let mut app = make_app();

        app.install_generation_fixture_with_pricing_diagnostics(vec![PricingDiagnostic::warning(
            "message wording says pricing unavailable and using cached pricing",
        )]);
        assert_eq!(app.pricing_warning(), None);

        app.install_generation_fixture_with_pricing_diagnostics(vec![
            PricingDiagnostic::cached_fallback("network error"),
        ]);
        assert_eq!(
            app.pricing_warning(),
            Some("Pricing refresh failed; using cached rates")
        );

        app.install_generation_fixture_with_pricing_diagnostics(vec![
            PricingDiagnostic::cached_fallback("stale cache"),
            PricingDiagnostic::unavailable("network error"),
        ]);
        assert_eq!(
            app.pricing_warning(),
            Some("Pricing unavailable; costs may be missing")
        );

        app.install_generation_fixture_with_pricing_diagnostics(Vec::new());
        assert_eq!(app.pricing_warning(), None);
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
    fn test_hourly_view_toggle_preserves_other_tab_interactions() {
        let mut app = make_app_with_models(5);
        app.usage_mut_for_test().daily = vec![
            daily_usage("2026-05-10", 1.0, vec![("old-model", "anthropic", 1.0)]),
            daily_usage("2026-05-17", 7.0, vec![("target", "openai", 7.0)]),
            daily_usage("2026-05-18", 3.0, vec![("other", "google", 3.0)]),
        ];

        app.switch_tab(Tab::Models);
        app.max_visible_items = 3;
        app.selected_index = 4;
        app.scroll_offset = 2;

        app.switch_tab(Tab::Daily);
        app.max_visible_items = 2;
        app.selected_index = 1;
        app.scroll_offset = 1;

        app.switch_tab(Tab::Hourly);
        app.selected_index = 3;
        app.scroll_offset = 1;
        app.hourly_profile_viewport.scroll = 4;

        app.handle_key_event(key(KeyCode::Char('v')));

        assert_eq!(app.hourly_view_mode, HourlyViewMode::Profile);
        assert_eq!(app.selected_index, 0);
        assert_eq!(app.scroll_offset, 0);
        assert_eq!(app.hourly_profile_viewport.scroll, 0);

        app.switch_tab(Tab::Models);
        assert_eq!(app.selected_index, 4);
        assert_eq!(app.scroll_offset, 2);
        assert_eq!(app.max_visible_items, 3);

        app.switch_tab(Tab::Daily);
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.scroll_offset, 1);
        assert_eq!(app.max_visible_items, 2);
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
}
