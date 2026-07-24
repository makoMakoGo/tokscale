use std::collections::BTreeSet;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::widgets::{format_cost, format_tokens, truncate_display_width};
use crate::tui::actions::{Action, ActionSet};
use crate::tui::app::{App, ClickAction, SortField, StatusTone, Tab};
use crate::tui::data::{build_period_usage, PeriodKind};
use crate::tui::presentation::SubscriptionPresentation;

#[derive(Clone, Copy)]
pub(super) struct SortControl {
    pub(super) field: SortField,
    pub(super) label: &'static str,
}

impl SortControl {
    pub(super) const fn new(field: SortField, label: &'static str) -> Self {
        Self { field, label }
    }
}

pub(super) struct FooterContent {
    sort_controls: Vec<SortControl>,
    sort_column_percent: u16,
    leading: Option<String>,
    summary: Line<'static>,
    help: Line<'static>,
    status: Option<Line<'static>>,
}

impl FooterContent {
    pub(super) fn new(
        sort_controls: Vec<SortControl>,
        summary: Line<'static>,
        help: Line<'static>,
    ) -> Self {
        Self {
            sort_controls,
            sort_column_percent: 40,
            leading: None,
            summary,
            help,
            status: None,
        }
    }

    pub(super) fn with_sort_column_percent(mut self, percent: u16) -> Self {
        self.sort_column_percent = percent.min(100);
        self
    }

    pub(super) fn with_leading(mut self, leading: String) -> Self {
        self.leading = Some(leading);
        self
    }

    pub(super) fn with_status(mut self, status: Line<'static>) -> Self {
        self.status = Some(status);
        self
    }
}

pub(super) fn standard_content(app: &App, actions: &ActionSet) -> FooterContent {
    debug_assert_ne!(app.current_tab, Tab::Sessions);
    let content = FooterContent::new(
        standard_sort_controls(actions),
        summary_row_line(app, actions),
        help_row_line(app, actions),
    );
    with_empty_scope(content, app, actions)
}

pub(super) fn subscription_content(
    app: &App,
    presentation: SubscriptionPresentation,
    actions: &ActionSet,
) -> FooterContent {
    FooterContent::new(
        Vec::new(),
        subscription_summary_line(app, presentation),
        subscription_help_line(app, actions),
    )
    .with_status(subscription_status_row_line(app))
}

pub(super) fn standard_sort_controls(actions: &ActionSet) -> Vec<SortControl> {
    [
        SortControl::new(SortField::Date, "Date"),
        SortControl::new(SortField::Cost, "Cost"),
        SortControl::new(SortField::Tokens, "Tokens"),
    ]
    .into_iter()
    .filter(|control| actions.contains(Action::Sort(control.field)))
    .collect()
}

pub(super) fn with_empty_scope(
    content: FooterContent,
    app: &App,
    actions: &ActionSet,
) -> FooterContent {
    if !actions.is_empty_view() {
        return content;
    }

    content.with_leading(format!("Scope: {}", super::empty_state::scope_summary(app)))
}

pub(super) fn render(frame: &mut Frame, app: &mut App, area: Rect, content: FooterContent) {
    let inner = render_shell(frame, app, area);
    if inner.is_empty() {
        return;
    }

    render_rows(frame, app, inner, content);
}

fn render_shell(frame: &mut Frame, app: &App, area: Rect) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.chrome.border))
        .style(app.theme.panel_style());

    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn render_rows(frame: &mut Frame, app: &mut App, inner: Rect, content: FooterContent) {
    // Split into 3 rows: main summary, help text, status.
    let row_constraints = if inner.height >= 3 {
        vec![
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else if inner.height >= 2 {
        vec![Constraint::Length(1), Constraint::Length(1)]
    } else {
        vec![Constraint::Length(1)]
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(inner);

    let FooterContent {
        sort_controls,
        sort_column_percent,
        leading,
        summary,
        help,
        status,
    } = content;
    render_main_row(
        frame,
        app,
        rows[0],
        &sort_controls,
        sort_column_percent,
        leading,
        summary,
    );

    if let Some(area) = rows.get(1).copied() {
        frame.render_widget(Paragraph::new(help), area);
    }

    if let Some(area) = rows.get(2).copied() {
        if let Some(status) = status {
            frame.render_widget(Paragraph::new(status), area);
        } else {
            render_status_row(frame, app, area);
        }
    }
}

pub(super) fn render_cold_loading(frame: &mut Frame, app: &App, area: Rect) {
    render_timed_activity(
        frame,
        app,
        area,
        super::loading::SCANNING_LOCAL_DATA,
        "Scanning",
        app.background_load_elapsed().unwrap_or_default().as_secs(),
    );
}

pub(super) fn render_timed_activity(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    message: &'static str,
    compact_message: &'static str,
    elapsed_secs: u64,
) {
    let inner = render_shell(frame, app, area);
    if inner.is_empty() {
        return;
    }

    render_centered_line(
        frame,
        inner,
        timed_activity_line(app, inner.width, message, compact_message, elapsed_secs),
    );
}

pub(super) fn render_cold_failed(frame: &mut Frame, app: &App, area: Rect, actions: &ActionSet) {
    debug_assert!(actions.contains(Action::RefreshLocal));
    debug_assert!(actions.contains(Action::Quit));

    let inner = render_shell(frame, app, area);
    if inner.is_empty() {
        return;
    }

    render_centered_line(frame, inner, cold_failed_line(app, inner.width));
}

fn render_centered_line(frame: &mut Frame, area: Rect, line: Line<'static>) {
    let row = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), row);
}

fn timed_activity_line(
    app: &App,
    width: u16,
    message: &'static str,
    compact_message: &'static str,
    elapsed_secs: u64,
) -> Line<'static> {
    const WAVE: &str = "~ ~";
    const MIN_WAVE_WIDTH: usize = 56;
    const TIMER_WIDTH: usize = 4;

    let elapsed = format!("{elapsed_secs}s");
    let elapsed = format!("{elapsed:>TIMER_WIDTH$}");
    let plain = format!("{message} ·{elapsed}");
    let decorated = format!("{WAVE}  {plain}  {WAVE}");
    let available = width as usize;

    if available >= MIN_WAVE_WIDTH && UnicodeWidthStr::width(decorated.as_str()) <= available {
        return Line::from(vec![
            Span::styled(
                WAVE.to_string(),
                Style::default().fg(app.theme.status.pending),
            ),
            Span::raw("  "),
            Span::styled(message, Style::default().fg(app.theme.text.muted)),
            Span::styled(" ·", Style::default().fg(app.theme.text.muted)),
            Span::styled(
                elapsed,
                Style::default()
                    .fg(app.theme.text.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                WAVE.to_string(),
                Style::default().fg(app.theme.status.pending),
            ),
        ]);
    }

    if UnicodeWidthStr::width(plain.as_str()) <= available {
        return Line::from(vec![
            Span::styled(message, Style::default().fg(app.theme.text.muted)),
            Span::styled(" ·", Style::default().fg(app.theme.text.muted)),
            Span::styled(
                elapsed,
                Style::default()
                    .fg(app.theme.text.primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
    }

    let compact = format!("{compact_message} ·{elapsed}");
    Line::from(Span::styled(
        truncate_display_width(&compact, available),
        Style::default().fg(app.theme.text.muted),
    ))
}

fn cold_failed_line(app: &App, width: u16) -> Line<'static> {
    const FULL: &str = "Scan failed · [r] Retry · [q] Quit";
    const ACTIONS: &str = "[r] Retry · [q] Quit";
    const COMPACT: &str = "r:retry · q:quit";
    let available = width as usize;

    if UnicodeWidthStr::width(FULL) <= available {
        return Line::from(vec![
            Span::styled(
                "Scan failed",
                Style::default()
                    .fg(app.theme.status.danger)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::default().fg(app.theme.text.muted)),
            Span::styled("[r] Retry", Style::default().fg(app.theme.chrome.focus)),
            Span::styled(" · ", Style::default().fg(app.theme.text.muted)),
            Span::styled("[q] Quit", Style::default().fg(app.theme.text.muted)),
        ]);
    }

    if UnicodeWidthStr::width(ACTIONS) <= available {
        return Line::from(vec![
            Span::styled("[r] Retry", Style::default().fg(app.theme.chrome.focus)),
            Span::styled(" · ", Style::default().fg(app.theme.text.muted)),
            Span::styled("[q] Quit", Style::default().fg(app.theme.text.muted)),
        ]);
    }

    let compact = if UnicodeWidthStr::width(COMPACT) <= available {
        COMPACT.to_string()
    } else if available >= 7 {
        "[r] [q]".to_string()
    } else {
        truncate_display_width("r q", available)
    };
    Line::from(Span::styled(
        compact,
        Style::default().fg(app.theme.text.muted),
    ))
}

fn render_main_row(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    sort_controls: &[SortControl],
    sort_column_percent: u16,
    leading: Option<String>,
    summary: Line<'static>,
) {
    let is_very_narrow = app.is_very_narrow();

    // Split into an optional leading/sort region and the summary.
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(sort_column_percent),
            Constraint::Percentage(100u16.saturating_sub(sort_column_percent)),
        ])
        .split(area);

    // The leading region is sortable only when the current ActionSet allows it.
    if !is_very_narrow && !sort_controls.is_empty() {
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            "Sort: ",
            Style::default().fg(app.theme.text.muted),
        ));
        let mut x_offset = chunks[0].x.saturating_add(6);

        for control in sort_controls {
            let is_active = app.sort_field == control.field;
            let style = if is_active {
                Style::default()
                    .fg(app.theme.chrome.current)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.text.muted)
            };

            spans.push(Span::styled(control.label, style));
            spans.push(Span::raw(" "));

            let label_width = control.label.width() as u16;
            let visible_width = label_width.min(chunks[0].right().saturating_sub(x_offset));
            if visible_width > 0 {
                app.add_click_area(
                    Rect::new(x_offset, chunks[0].y, visible_width, 1),
                    ClickAction::Sort(control.field),
                );
            }
            x_offset = x_offset.saturating_add(label_width).saturating_add(1);
        }

        frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
    } else if let Some(leading) = leading {
        frame.render_widget(
            Paragraph::new(truncate_display_width(&leading, chunks[0].width as usize))
                .style(Style::default().fg(app.theme.text.muted)),
            chunks[0],
        );
    }

    frame.render_widget(
        Paragraph::new(summary).alignment(Alignment::Right),
        chunks[1],
    );
}

pub(super) fn summary_row_line(app: &App, actions: &ActionSet) -> Line<'static> {
    let is_very_narrow = app.is_very_narrow();
    let mut right_spans: Vec<Span> = Vec::new();

    // Total tokens
    let total_tokens = app.data.total_tokens;
    right_spans.push(Span::styled(
        format_tokens(total_tokens),
        Style::default().fg(app.theme.metrics.tokens),
    ));
    if !is_very_narrow && !actions.is_empty_view() {
        right_spans.push(Span::styled(
            " tokens",
            Style::default().fg(app.theme.text.muted),
        ));
    }

    right_spans.push(Span::styled(
        " | ",
        Style::default().fg(app.theme.text.muted),
    ));

    // Total cost
    right_spans.push(Span::styled(
        format_cost(app.data.total_cost),
        Style::default()
            .fg(app.theme.metrics.cost)
            .add_modifier(Modifier::BOLD),
    ));

    // Current list count
    if !is_very_narrow {
        let count_label = current_count_label(app);
        right_spans.push(Span::styled(
            count_label,
            Style::default().fg(app.theme.text.muted),
        ));
    }

    Line::from(right_spans)
}

fn subscription_summary_line(app: &App, presentation: SubscriptionPresentation) -> Line<'static> {
    match presentation {
        SubscriptionPresentation::ColdFetching => Line::default(),
        SubscriptionPresentation::Prompt => {
            let configured = app.enabled_subscription_provider_count();
            if configured == 0 {
                Line::from(Span::styled(
                    "No providers configured",
                    Style::default().fg(app.theme.text.muted),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        count_label(configured, "provider", "providers"),
                        Style::default().fg(app.theme.metrics.total),
                    ),
                    Span::styled(" configured", Style::default().fg(app.theme.text.muted)),
                ])
            }
        }
        SubscriptionPresentation::Empty { .. } if app.subscription_usage.is_empty() => {
            Line::from(Span::styled(
                "No subscription results",
                Style::default().fg(app.theme.text.muted),
            ))
        }
        SubscriptionPresentation::Empty { .. } | SubscriptionPresentation::Results { .. } => {
            let providers = app.subscription_usage.len();
            let limits = app
                .subscription_usage
                .iter()
                .map(|output| output.metrics.len())
                .sum();
            let errors = app.subscription_usage_errors.len();
            let mut spans = vec![
                Span::styled(
                    count_label(providers, "provider", "providers"),
                    Style::default().fg(app.theme.metrics.total),
                ),
                Span::styled(" · ", Style::default().fg(app.theme.text.muted)),
                Span::styled(
                    count_label(limits, "limit", "limits"),
                    Style::default().fg(app.theme.text.primary),
                ),
            ];
            if errors > 0 {
                spans.push(Span::styled(
                    " · ",
                    Style::default().fg(app.theme.text.muted),
                ));
                spans.push(Span::styled(
                    count_label(errors, "error", "errors"),
                    Style::default().fg(app.theme.status.danger),
                ));
            }
            Line::from(spans)
        }
    }
}

fn count_label(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn current_count_label(app: &App) -> String {
    match app.current_tab {
        Tab::Overview => {
            let mut models = BTreeSet::new();
            let mut clients = BTreeSet::new();
            for day in &app.data.daily {
                for (client, client_info) in &day.client_breakdown {
                    clients.insert(client.as_str());
                    for model in client_info.models.values() {
                        models.insert(model.model_id.as_str());
                    }
                }
            }
            format!(
                " ({} models · {} clients · {} days)",
                models.len(),
                clients.len(),
                app.data.daily.len()
            )
        }
        Tab::Models if app.is_model_detail_active() => {
            format!(" ({} provider rows)", app.get_sorted_models().len())
        }
        Tab::Models => format!(" ({} models)", app.data.models.len()),
        Tab::Agents => format!(" ({} agents)", app.data.agents.len()),
        Tab::Daily if app.is_daily_detail_active() => {
            format!(" ({} models)", app.get_sorted_daily_detail_rows().len())
        }
        Tab::Monthly if app.is_period_detail_active_for_kind(PeriodKind::Monthly) => {
            format!(" ({} models)", app.get_sorted_period_detail_rows().len())
        }
        Tab::Weekly if app.is_period_detail_active_for_kind(PeriodKind::Weekly) => {
            format!(" ({} models)", app.get_sorted_period_detail_rows().len())
        }
        Tab::Monthly => format!(
            " ({} months)",
            build_period_usage(&app.data.daily, PeriodKind::Monthly).len()
        ),
        Tab::Weekly => format!(
            " ({} weeks)",
            build_period_usage(&app.data.daily, PeriodKind::Weekly).len()
        ),
        Tab::Daily => format!(" ({} days)", app.data.daily.len()),
        Tab::Hourly => format!(" ({} hours)", app.data.hourly.len()),
        Tab::Sessions => unreachable!("sessions footer supplies its own summary"),
        Tab::Stats | Tab::Usage => String::new(),
    }
}

pub(super) fn help_row_line(app: &App, actions: &ActionSet) -> Line<'static> {
    action_help_row_line(app, actions, None)
}

fn subscription_help_line(app: &App, actions: &ActionSet) -> Line<'static> {
    let narrow = app.is_very_narrow();
    let separator = if narrow { "·" } else { " · " };
    let mut items = Vec::<(String, Style)>::new();

    if actions.contains(Action::RefreshSubscription) {
        items.push((
            (if narrow { "[u]" } else { "[u:refresh]" }).to_string(),
            Style::default().fg(app.theme.chrome.focus),
        ));
    }
    if actions.contains(Action::Scroll) {
        items.push((
            (if narrow { "↑↓" } else { "↑↓ scroll" }).to_string(),
            Style::default().fg(app.theme.text.muted),
        ));
    }
    if actions.contains(Action::PreviousTab) || actions.contains(Action::NextTab) {
        items.push((
            (if narrow { "←→" } else { "←→/tab view" }).to_string(),
            Style::default().fg(app.theme.text.muted),
        ));
    }
    if actions.contains(Action::Theme) {
        items.push((
            (if narrow { "[p]" } else { "[p:theme]" }).to_string(),
            Style::default().fg(app.theme.chrome.focus),
        ));
    }
    if actions.contains(Action::Quit) {
        items.push(("q".to_string(), Style::default().fg(app.theme.text.muted)));
    }

    let mut spans = Vec::new();
    for (label, style) in items {
        if !spans.is_empty() {
            spans.push(Span::styled(
                separator.to_string(),
                Style::default().fg(app.theme.text.muted),
            ));
        }
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
}

pub(super) fn action_help_row_line(
    app: &App,
    actions: &ActionSet,
    toggle_target: Option<&str>,
) -> Line<'static> {
    let is_very_narrow = app.is_very_narrow();
    debug_assert_ne!(app.current_tab, Tab::Usage);

    let separator = if is_very_narrow { "·" } else { " • " };
    let mut spans = Vec::new();
    let mut emitted_navigation = false;
    let mut emitted_sort = false;

    for action in actions.iter() {
        if actions.is_empty_view()
            && !matches!(
                action,
                Action::Clients | Action::RefreshLocal | Action::PreviousTab | Action::NextTab
            )
        {
            continue;
        }

        let label = match action {
            Action::PreviousTab | Action::NextTab => {
                if emitted_navigation {
                    continue;
                }
                emitted_navigation = true;
                if is_very_narrow {
                    "←→".to_string()
                } else {
                    "←→/tab view".to_string()
                }
            }
            Action::Sort(_) => {
                if emitted_sort {
                    continue;
                }
                emitted_sort = true;
                if is_very_narrow {
                    "d/t/c".to_string()
                } else {
                    "[d/t/c:sort]".to_string()
                }
            }
            Action::Scroll => {
                if is_very_narrow {
                    "↑↓".to_string()
                } else {
                    "↑↓ scroll".to_string()
                }
            }
            Action::OpenDetails => {
                if is_very_narrow {
                    "↵".to_string()
                } else if app.current_tab == Tab::Sessions {
                    "[enter:sessions]".to_string()
                } else {
                    "[enter:details]".to_string()
                }
            }
            Action::Back => {
                if is_very_narrow {
                    "esc".to_string()
                } else {
                    "[esc:back]".to_string()
                }
            }
            Action::JumpToday => {
                if is_very_narrow {
                    "j".to_string()
                } else {
                    "[j:today]".to_string()
                }
            }
            Action::ToggleView => toggle_action_label(app, toggle_target, is_very_narrow),
            Action::Clients => {
                if is_very_narrow {
                    "[s]".to_string()
                } else {
                    "[s:clients]".to_string()
                }
            }
            Action::GroupBy => {
                if is_very_narrow {
                    "[g]".to_string()
                } else {
                    format!("[g:{}]", app.group_by.borrow())
                }
            }
            Action::Theme => {
                if is_very_narrow {
                    "[p]".to_string()
                } else {
                    format!("[p:{}]", app.theme.name.as_str())
                }
            }
            Action::ToggleAutoRefresh => {
                if is_very_narrow {
                    "[R]".to_string()
                } else if app.auto_refresh {
                    format!("[R:local auto {}s]", app.auto_refresh_interval.as_secs())
                } else {
                    "[R:local auto off]".to_string()
                }
            }
            Action::RefreshLocal => {
                if is_very_narrow {
                    "[r]".to_string()
                } else {
                    "[r:rescan]".to_string()
                }
            }
            Action::IncreaseRefreshInterval
            | Action::DecreaseRefreshInterval
            | Action::RefreshSubscription
            | Action::Copy => continue,
            Action::Export => "e".to_string(),
            Action::Quit => "q".to_string(),
        };

        if !spans.is_empty() {
            spans.push(Span::styled(
                separator.to_string(),
                Style::default().fg(app.theme.text.muted),
            ));
        }
        spans.push(Span::styled(label, action_style(app, action)));
    }

    Line::from(spans)
}

fn toggle_action_label(app: &App, target: Option<&str>, narrow: bool) -> String {
    let (key, target) = match app.current_tab {
        Tab::Overview => (
            'h',
            match app.chart_granularity {
                crate::tui::app::ChartGranularity::Daily => "hourly",
                crate::tui::app::ChartGranularity::Hourly => "daily",
            },
        ),
        Tab::Daily => ('v', target.unwrap_or("view")),
        Tab::Hourly => (
            'v',
            match app.hourly_view_mode {
                crate::tui::app::HourlyViewMode::Table => "profile",
                crate::tui::app::HourlyViewMode::Profile => "table",
            },
        ),
        _ => ('v', target.unwrap_or("view")),
    };
    if narrow {
        key.to_string()
    } else {
        format!("[{key}:{target}]")
    }
}

fn action_style(app: &App, action: Action) -> Style {
    let color = match action {
        Action::Sort(_) => app.theme.chrome.current,
        Action::Clients | Action::GroupBy | Action::Theme => app.theme.chrome.focus,
        Action::ToggleAutoRefresh if app.auto_refresh => app.theme.status.success,
        Action::OpenDetails
        | Action::Back
        | Action::JumpToday
        | Action::ToggleView
        | Action::RefreshLocal => app.theme.chrome.focus,
        Action::Scroll
        | Action::PreviousTab
        | Action::NextTab
        | Action::ToggleAutoRefresh
        | Action::IncreaseRefreshInterval
        | Action::DecreaseRefreshInterval
        | Action::RefreshSubscription
        | Action::Copy
        | Action::Export
        | Action::Quit => app.theme.text.muted,
    };
    Style::default().fg(color)
}

pub(super) fn render_status_row(frame: &mut Frame, app: &App, area: Rect) {
    debug_assert_ne!(app.current_tab, Tab::Usage);
    let paragraph = Paragraph::new(status_row_line(app));
    frame.render_widget(paragraph, area);
}

fn status_style(app: &App, tone: StatusTone) -> Style {
    let color = match tone {
        StatusTone::Info => app.theme.status.info,
        StatusTone::Success => app.theme.status.success,
        StatusTone::Warning => app.theme.status.warning,
        StatusTone::Danger => app.theme.status.danger,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn status_row_line(app: &App) -> Line<'static> {
    if let Some(warning) = app.cache_persistence_warning() {
        return Line::from(Span::styled(
            warning.to_string(),
            Style::default()
                .fg(app.theme.status.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Cold loading and cold failure use the centered footer presentation.
    // The standard status row stays reserved for installed generations.
    if app.is_cold_loading() || app.is_cold_failed() {
        return Line::default();
    }

    let mut spans: Vec<Span> = Vec::new();

    if app.background_loading {
        spans.push(Span::styled(
            "Refreshing cached data in background...",
            Style::default().fg(app.theme.status.pending),
        ));
    } else if let Some(ref msg) = app.status_message {
        spans.push(Span::styled(
            msg.clone(),
            status_style(app, app.status_message_tone()),
        ));
    } else if let Some(warning) = app.pricing_warning() {
        spans.push(Span::styled(
            warning,
            Style::default()
                .fg(app.theme.status.warning)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        let elapsed = app.last_refresh.elapsed();
        let ago = if elapsed.as_secs() < 60 {
            format!("{}s ago", elapsed.as_secs())
        } else if elapsed.as_secs() < 3600 {
            format!("{}m ago", elapsed.as_secs() / 60)
        } else {
            format!("{}h ago", elapsed.as_secs() / 3600)
        };
        spans.push(Span::styled(
            format!("Last updated: {}", ago),
            Style::default().fg(app.theme.text.muted),
        ));

        if app.auto_refresh {
            spans.push(Span::styled(
                format!(" • Auto: {}s", app.auto_refresh_interval.as_secs()),
                Style::default().fg(app.theme.text.muted),
            ));
        }
    }

    Line::from(spans)
}

fn subscription_status_row_line(app: &App) -> Line<'static> {
    let (text, style) = if app.is_fetching_usage() {
        (
            "Refreshing subscription usage...".to_string(),
            Style::default()
                .fg(app.theme.status.pending)
                .add_modifier(Modifier::BOLD),
        )
    } else if let Some(msg) = subscription_status_message(app) {
        (
            msg.to_string(),
            status_style(app, app.subscription_status_message_tone()),
        )
    } else if let Some(msg) = app.general_status_message() {
        (
            msg.to_string(),
            status_style(app, app.status_message_tone()),
        )
    } else if let Some(updated_at) = app.last_subscription_usage_check {
        (
            format!(
                "Subscription checked: {}",
                elapsed_label(updated_at.elapsed())
            ),
            Style::default().fg(app.theme.text.muted),
        )
    } else if !app.subscription_usage.is_empty() {
        (
            if app.has_enabled_subscription_providers() {
                "Subscription usage loaded from cache".to_string()
            } else {
                "Showing cached subscription usage; no remote providers enabled".to_string()
            },
            Style::default().fg(app.theme.text.muted),
        )
    } else if !app.has_enabled_subscription_providers() {
        (
            "No remote subscription providers enabled; configure usageProviders".to_string(),
            Style::default().fg(app.theme.text.muted),
        )
    } else {
        (
            "Press u to refresh subscription usage".to_string(),
            Style::default().fg(app.theme.text.muted),
        )
    };

    Line::from(vec![Span::styled(text, style)])
}

fn subscription_status_message(app: &App) -> Option<&str> {
    app.subscription_status_message.as_deref()
}

fn elapsed_label(elapsed: std::time::Duration) -> String {
    if elapsed.as_secs() < 60 {
        format!("{}s ago", elapsed.as_secs())
    } else if elapsed.as_secs() < 3600 {
        format!("{}m ago", elapsed.as_secs() / 60)
    } else {
        format!("{}h ago", elapsed.as_secs() / 3600)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tui::app::{ProjectionBackend, TuiConfig};
    use crate::tui::data::{DailyUsage, ModelUsage, TokenBreakdown, UsageData};
    use crate::tui::settings::Settings;
    use crate::tui::subscription_usage::{UsageMetric, UsageOutput, UsageProviderId};
    use chrono::NaiveDate;

    fn make_app_on(tab: Tab) -> App {
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
            usage_tab_enabled: true,
            ..Settings::default()
        };
        App::new_with_cached_data_and_settings(config, Some(UsageData::default()), settings)
            .unwrap()
    }

    fn line_text(line: Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn installed_app_on(tab: Tab) -> App {
        let mut app = make_app_on(tab);
        app.projection_backend = Some(ProjectionBackend::Memory(tokscale_core::TuiAcc::new()));
        app
    }

    fn nonempty_installed_app_on(tab: Tab) -> App {
        let mut app = installed_app_on(tab);
        app.data.daily.push(DailyUsage {
            date: NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
            tokens: TokenBreakdown::default(),
            cost: 0.0,
            client_breakdown: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
        });
        app.data.models.push(ModelUsage {
            model_id: "test-model".to_string(),
            display_name: "Test Model".to_string(),
            provider: "test-provider".to_string(),
            client: "codex".to_string(),
            workspace_key: None,
            workspace_label: None,
            tokens: TokenBreakdown::default(),
            cost: 0.0,
            session_count: 1,
        });
        app
    }

    fn help_text(app: &App) -> String {
        let state = crate::tui::view_state::ViewState::default();
        let presentation = crate::tui::presentation::Presentation::for_view(app, &state);
        let actions = ActionSet::for_view(app, &state, presentation);
        match presentation {
            crate::tui::presentation::Presentation::Subscription(_) => {
                line_text(subscription_help_line(app, &actions))
            }
            _ => line_text(help_row_line(app, &actions)),
        }
    }

    #[test]
    fn footer_actions_use_semantic_interaction_and_status_colors() {
        let mut app = nonempty_installed_app_on(Tab::Overview);

        assert_eq!(
            action_style(&app, Action::Sort(SortField::Date)).fg,
            Some(app.theme.chrome.current)
        );
        assert_eq!(
            action_style(&app, Action::Clients).fg,
            Some(app.theme.chrome.focus)
        );
        assert_eq!(
            action_style(&app, Action::Theme).fg,
            Some(app.theme.chrome.focus)
        );
        assert_eq!(
            action_style(&app, Action::Scroll).fg,
            Some(app.theme.text.muted)
        );

        app.auto_refresh = true;
        assert_eq!(
            action_style(&app, Action::ToggleAutoRefresh).fg,
            Some(app.theme.status.success)
        );
    }

    #[test]
    fn standard_status_messages_use_their_semantic_tone_colors() {
        let mut app = installed_app_on(Tab::Overview);

        app.set_status("Informational status");
        assert_eq!(
            status_row_line(&app).spans[0].style.fg,
            Some(app.theme.status.info)
        );
        app.set_local_report_status("Local informational status");
        assert_eq!(
            status_row_line(&app).spans[0].style.fg,
            Some(app.theme.status.info)
        );

        for (tone, expected) in [
            (StatusTone::Success, app.theme.status.success),
            (StatusTone::Warning, app.theme.status.warning),
            (StatusTone::Danger, app.theme.status.danger),
        ] {
            app.set_status_with_tone("Transient status", tone);
            assert_eq!(status_row_line(&app).spans[0].style.fg, Some(expected));
        }
    }

    #[test]
    fn subscription_status_messages_use_their_semantic_tone_colors() {
        let mut app = make_app_on(Tab::Usage);

        for (tone, expected) in [
            (StatusTone::Success, app.theme.status.success),
            (StatusTone::Warning, app.theme.status.warning),
            (StatusTone::Danger, app.theme.status.danger),
        ] {
            app.set_subscription_status_with_tone("Subscription status", tone);
            assert_eq!(
                subscription_status_row_line(&app).spans[0].style.fg,
                Some(expected)
            );
        }
    }

    #[test]
    fn cold_failure_footer_uses_danger_focus_and_muted_roles() {
        let app = make_app_on(Tab::Overview);
        let line = cold_failed_line(&app, 80);

        assert_eq!(line.spans[0].style.fg, Some(app.theme.status.danger));
        assert_eq!(line.spans[1].style.fg, Some(app.theme.text.muted));
        assert_eq!(line.spans[2].style.fg, Some(app.theme.chrome.focus));
        assert_eq!(line.spans[4].style.fg, Some(app.theme.text.muted));
    }

    #[test]
    fn test_current_count_label_matches_active_tab() {
        assert_eq!(
            current_count_label(&make_app_on(Tab::Overview)),
            " (0 models · 0 clients · 0 days)"
        );
        assert_eq!(
            current_count_label(&make_app_on(Tab::Models)),
            " (0 models)"
        );
        assert_eq!(
            current_count_label(&make_app_on(Tab::Agents)),
            " (0 agents)"
        );
        assert_eq!(
            current_count_label(&make_app_on(Tab::Monthly)),
            " (0 months)"
        );
        assert_eq!(current_count_label(&make_app_on(Tab::Weekly)), " (0 weeks)");
        assert_eq!(current_count_label(&make_app_on(Tab::Daily)), " (0 days)");
        assert_eq!(current_count_label(&make_app_on(Tab::Hourly)), " (0 hours)");
        assert_eq!(current_count_label(&make_app_on(Tab::Stats)), "");
    }

    #[test]
    fn usage_help_row_only_shows_subscription_and_shell_actions() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);

        let text = help_text(&app);

        assert!(text.contains("[u:refresh]"));
        assert!(text.contains("←→/tab view"));
        assert!(text.contains("[p:theme]"));
        assert!(text.ends_with('q'));
        for local in ["[r:", "[R:", "[e:"] {
            assert!(!text.contains(local), "{text}");
        }
    }

    #[test]
    fn usage_help_row_hides_subscription_refresh_without_enabled_providers() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(Vec::new());

        let text = help_text(&app);

        assert!(!text.contains("[u:refresh]"));
        assert!(text.contains("←→/tab view"));
        assert!(text.contains("[p:theme]"));
        assert!(text.ends_with('q'));
        assert!(!text.contains("local"));
    }

    #[test]
    fn narrow_usage_help_row_hides_u_without_enabled_providers() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.terminal_width = 50;
        app.set_subscription_provider_ids_for_test(Vec::new());

        let text = help_text(&app);

        assert!(!text.contains("[u]"));
        assert!(text.contains("←→"));
        assert!(text.contains("[p]"));
        assert!(text.ends_with('q'));
        assert!(!text.contains("local"));
    }

    #[test]
    fn group_by_hint_only_shows_on_group_keyed_tabs() {
        for tab in [Tab::Models, Tab::Daily, Tab::Monthly, Tab::Weekly] {
            let text = help_text(&nonempty_installed_app_on(tab));
            assert!(text.contains("[g:"), "expected [g: hint on {tab:?}");
        }
        for tab in [
            Tab::Overview,
            Tab::Stats,
            Tab::Hourly,
            Tab::Usage,
            Tab::Sessions,
            Tab::Agents,
        ] {
            let text = help_text(&nonempty_installed_app_on(tab));
            assert!(!text.contains("[g:"), "unexpected [g: hint on {tab:?}");
        }
    }

    #[test]
    fn narrow_group_by_hint_only_shows_on_group_keyed_tabs() {
        for (tab, expected) in [
            (Tab::Models, true),
            (Tab::Daily, true),
            (Tab::Monthly, true),
            (Tab::Weekly, true),
            (Tab::Overview, false),
            (Tab::Stats, false),
            (Tab::Hourly, false),
            (Tab::Sessions, false),
            (Tab::Agents, false),
        ] {
            let mut app = nonempty_installed_app_on(tab);
            app.terminal_width = 50;
            let text = help_text(&app);
            assert_eq!(text.contains("[g]"), expected, "tab {tab:?}");
        }
    }

    #[test]
    fn cold_local_generation_states_leave_the_footer_status_empty() {
        let mut app = make_app_on(Tab::Overview);
        app.set_background_loading(true);
        assert_eq!(line_text(status_row_line(&app)), "");

        app.set_background_loading(false);
        app.set_error(Some("injected cold failure".to_string()));
        app.set_local_report_status("Error: injected cold failure");
        assert_eq!(line_text(status_row_line(&app)), "");
    }

    #[test]
    fn empty_installed_generation_uses_the_warm_refresh_status() {
        let mut app = make_app_on(Tab::Overview);
        app.projection_backend = Some(ProjectionBackend::Memory(tokscale_core::TuiAcc::new()));
        app.set_background_loading(true);

        assert_eq!(
            line_text(status_row_line(&app)),
            "Refreshing cached data in background..."
        );
    }

    #[test]
    fn usage_status_row_uses_subscription_check_clock() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.last_refresh = std::time::Instant::now() - std::time::Duration::from_secs(600);
        app.last_subscription_usage_check =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(10));

        let text = line_text(subscription_status_row_line(&app));

        assert!(text.contains("Subscription checked:"));
        assert!(!text.contains("Last updated"));
        assert!(!text.contains("Auto:"));
    }

    #[test]
    fn usage_status_row_does_not_reuse_local_cache_status() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);
        app.set_local_report_status("Loaded from cache");

        let text = line_text(subscription_status_row_line(&app));

        assert_eq!(text, "Press u to refresh subscription usage");
    }

    #[test]
    fn usage_status_row_ignores_local_usage_status() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);
        app.set_local_report_status("Jumped to today's usage");

        let text = line_text(subscription_status_row_line(&app));

        assert_eq!(text, "Press u to refresh subscription usage");
    }

    #[test]
    fn usage_status_row_shows_general_action_status() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);
        app.set_status("Theme save failed: permission denied");

        let text = line_text(subscription_status_row_line(&app));

        assert_eq!(text, "Theme save failed: permission denied");
    }

    #[test]
    fn local_report_warnings_do_not_cross_into_usage_status() {
        let mut app = make_app_on(Tab::Models);
        app.status_message = None;
        app.status_message_time = None;
        app.set_pricing_diagnostics(&[format!(
            "{}: network error",
            tokscale_core::pricing::DIAGNOSTIC_PRICING_UNAVAILABLE
        )]);

        assert_eq!(
            line_text(status_row_line(&app)),
            "Pricing unavailable; costs may be missing"
        );

        app.set_cache_persistence_warning(Some(
            "Cache persistence warning: permission denied".to_string(),
        ));
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(vec![UsageProviderId::Codex]);
        assert_eq!(
            line_text(subscription_status_row_line(&app)),
            "Press u to refresh subscription usage"
        );
    }

    #[test]
    fn usage_status_row_reports_cache_display_mode_without_providers() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(Vec::new());
        app.subscription_usage.push(UsageOutput {
            provider: "Codex".to_string(),
            account: None,
            plan: None,
            email: None,
            metrics: vec![UsageMetric {
                label: "Weekly".to_string(),
                used_percent: 10.0,
                remaining_percent: 90.0,
                remaining_label: None,
                resets_at: None,
            }],
        });

        let text = line_text(subscription_status_row_line(&app));

        assert_eq!(
            text,
            "Showing cached subscription usage; no remote providers enabled"
        );
    }

    #[test]
    fn usage_status_row_reports_missing_provider_configuration() {
        let mut app = make_app_on(Tab::Overview);
        app.current_tab = Tab::Usage;
        app.set_subscription_provider_ids_for_test(Vec::new());

        let text = line_text(subscription_status_row_line(&app));

        assert_eq!(
            text,
            "No remote subscription providers enabled; configure usageProviders"
        );
    }

    #[test]
    fn cache_persistence_warning_stays_visible_over_transient_status() {
        let mut app = make_app_on(Tab::Models);
        app.set_status("Data loaded");
        app.set_cache_persistence_warning(Some(
            "Cache persistence warning: permission denied".to_string(),
        ));

        let text = line_text(status_row_line(&app));

        assert_eq!(text, "Cache persistence warning: permission denied");
    }
}
