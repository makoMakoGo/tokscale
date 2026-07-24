use std::collections::BTreeMap;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::bar_chart::{render_stacked_bar_chart, ModelSegment, StackedBarData};
use super::empty_state;
use crate::tui::actions::ActionSet;
use crate::tui::app::{App, ChartGranularity};
use crate::tui::presentation::EmptySubject;

#[derive(Debug, Clone, Default)]
struct ModelAggregate {
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, Default)]
struct OverviewData {
    models: BTreeMap<String, ModelAggregate>,
}

pub(crate) fn render(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    empty: Option<EmptySubject>,
    actions: &ActionSet,
) -> Rect {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(app.theme.panel_style()), area);

    if area.is_empty() {
        return area;
    }

    app.set_max_visible_items(1);
    let chart_height = if area.height >= 24 {
        (area.height * 2 / 5).max(8)
    } else {
        (area.height / 2).max(6)
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            // The box border absorbs the old standalone legend row.
            Constraint::Length((chart_height + 1).min(area.height)),
            Constraint::Min(0),
        ])
        .split(area);

    let title = if app.is_very_narrow() {
        " Tokens "
    } else {
        " Tokens per Day "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.chrome.border))
        .title(Span::styled(
            title,
            Style::default()
                .fg(app.theme.chrome.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .style(app.theme.panel_style());
    let inner = block.inner(chunks[0]);
    frame.render_widget(block, chunks[0]);

    if empty_state::render_if(
        frame,
        app,
        inner.inner(Margin {
            horizontal: 1,
            vertical: 0,
        }),
        empty,
        actions,
    ) {
        return chunks[1];
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let inset = Margin {
        horizontal: 1,
        vertical: 0,
    };
    render_chart(frame, app, sections[0].inner(inset));
    render_legend(frame, app, sections[1].inner(inset));
    chunks[1]
}

fn collect_overview_data(app: &App) -> OverviewData {
    let mut overview = OverviewData::default();

    for day in &app.data.daily {
        for client in day.client_breakdown.values() {
            for model in client.models.values() {
                let entry = overview.models.entry(model.model_id.clone()).or_default();
                entry.tokens = entry
                    .tokens
                    .checked_add(model.tokens.total())
                    .expect("overview model token total exceeds u64::MAX");
                entry.cost += model.cost;
            }
        }
    }

    overview
}

fn render_chart(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let data: Vec<StackedBarData> = match app.chart_granularity {
        ChartGranularity::Daily => app
            .data
            .daily
            .iter()
            .take(60)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|day| {
                let mut models = BTreeMap::<String, ModelAggregate>::new();
                for client in day.client_breakdown.values() {
                    for model in client.models.values() {
                        let entry = models.entry(model.model_id.clone()).or_default();
                        entry.tokens = entry
                            .tokens
                            .checked_add(model.tokens.total())
                            .expect("overview chart token total exceeds u64::MAX");
                    }
                }

                StackedBarData {
                    date: day.date.format("%m/%d").to_string(),
                    models: models
                        .into_iter()
                        .map(|(model, aggregate)| ModelSegment {
                            color: app.model_color(&model),
                            model_id: model,
                            tokens: aggregate.tokens,
                        })
                        .collect(),
                    total: day.tokens.total(),
                }
            })
            .collect(),
        ChartGranularity::Hourly => app
            .data
            .hourly
            .iter()
            .take(60)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|hour| {
                let mut models = BTreeMap::<String, ModelAggregate>::new();
                for model in hour.models.values() {
                    let entry = models.entry(model.model_id.clone()).or_default();
                    entry.tokens = entry
                        .tokens
                        .checked_add(model.tokens.total())
                        .expect("overview hourly chart token total exceeds u64::MAX");
                }

                StackedBarData {
                    date: hour.datetime.format("%d %H:%M").to_string(),
                    models: models
                        .into_iter()
                        .map(|(model, aggregate)| ModelSegment {
                            color: app.model_color(&model),
                            model_id: model,
                            tokens: aggregate.tokens,
                        })
                        .collect(),
                    total: hour.tokens.total(),
                }
            })
            .collect(),
    };

    render_stacked_bar_chart(frame, app, area, &data);
}

fn render_legend(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let overview = collect_overview_data(app);
    let mut models: Vec<_> = overview.models.iter().collect();
    models.sort_by(|(left_name, left), (right_name, right)| {
        right
            .tokens
            .cmp(&left.tokens)
            .then_with(|| right.cost.total_cmp(&left.cost))
            .then_with(|| left_name.cmp(right_name))
    });

    let limit = if app.is_narrow() { 3 } else { 5 };
    let name_width = if app.is_narrow() { 12 } else { 18 };
    let total_models = models.len();
    let visible_count = visible_legend_count(
        models.iter().map(|(model, _)| model.as_str()),
        limit,
        name_width,
        area.width as usize,
    );
    let mut spans = Vec::new();
    for (index, (model, _)) in models.into_iter().take(visible_count).enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            "■",
            Style::default().fg(app.model_color(model)),
        ));
        spans.push(Span::raw(format!(
            " {}",
            truncate_string(model, name_width)
        )));
    }

    // Models that did not fit collapse into a muted `+N` suffix. It follows
    // the last visible model — or renders alone when no model fits at all, so
    // the legend never goes blank while models exist.
    let hidden_count = total_models - visible_count;
    if hidden_count > 0 {
        if visible_count > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("+{hidden_count}"),
            Style::default().fg(app.theme.text.muted),
        ));
    }

    if !spans.is_empty() {
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn visible_legend_count<'a>(
    models: impl IntoIterator<Item = &'a str>,
    limit: usize,
    name_width: usize,
    available_width: usize,
) -> usize {
    let models: Vec<&str> = models.into_iter().collect();
    let mut used_width = 0usize;
    let mut visible_count = 0;

    for model in models.iter().copied().take(limit) {
        let display_name = truncate_string(model, name_width);
        let item_width = "■".width() + 1 + display_name.width();
        let gap_width = if visible_count == 0 { 0 } else { "  ".width() };
        // While accepting this item would still leave hidden models, reserve
        // room for the `+N` suffix so it too renders completely.
        let suffix_width = if visible_count + 1 < models.len() {
            "  ".width() + format!("+{}", models.len() - visible_count - 1).width()
        } else {
            0
        };
        let required_width = gap_width + item_width + suffix_width;
        if used_width.saturating_add(required_width) > available_width {
            break;
        }

        used_width += gap_width + item_width;
        visible_count += 1;
    }

    visible_count
}

fn truncate_string(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    format!("{}…", value.chars().take(max_chars - 1).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiConfig;
    use crate::tui::data::{DailyClientInfo, DailyModelInfo, DailyUsage, TokenBreakdown};
    use chrono::NaiveDate;
    use ratatui::{backend::TestBackend, Terminal};

    fn make_app(width: u16) -> App {
        let config = TuiConfig {
            theme: Some("blue".to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();
        app.terminal_width = width;
        app
    }

    fn app_with_models(width: u16, model_names: &[&str]) -> App {
        let mut app = make_app(width);
        let mut models = BTreeMap::new();
        for name in model_names {
            models.insert(
                name.to_string(),
                DailyModelInfo {
                    provider: "provider".to_string(),
                    model_id: name.to_string(),
                    display_name: name.to_string(),
                    workspace_key: None,
                    workspace_label: None,
                    tokens: TokenBreakdown {
                        input: 100,
                        output: 10,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    cost: 1.0,
                    messages: 1,
                },
            );
        }
        let mut client_breakdown = BTreeMap::new();
        client_breakdown.insert(
            "claude".to_string(),
            DailyClientInfo {
                tokens: TokenBreakdown::default(),
                cost: 1.0,
                models,
            },
        );
        app.data.daily = vec![DailyUsage {
            date: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            tokens: TokenBreakdown::default(),
            cost: 1.0,
            client_breakdown,
            message_count: 1,
            turn_count: 1,
        }];
        app
    }

    fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer
            .content()
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn legend_only_includes_complete_items_that_fit() {
        // '■' + space + 18-cell name = 20 cells per item, gaps are 2 cells, and
        // while models stay hidden 4 more cells are reserved for the `+N` suffix.
        let models = ["123456789012345678"; 5];
        assert_eq!(visible_legend_count(models, 5, 18, 67), 2);
        assert_eq!(visible_legend_count(models, 5, 18, 68), 3);
        assert_eq!(visible_legend_count(models, 5, 18, 89), 3);
        assert_eq!(visible_legend_count(models, 5, 18, 90), 4);
        // Once every model fits, the last item needs no suffix reservation.
        assert_eq!(visible_legend_count(models, 5, 18, 107), 4);
        assert_eq!(visible_legend_count(models, 5, 18, 108), 5);
    }

    #[test]
    fn legend_suffix_width_grows_with_the_hidden_count() {
        // 2-cell names make each item 4 cells ('■' + space + name), gaps 2.
        let models = ["aa"; 12];
        // At 28 cells the fifth model plus its `+7` reservation does not fit…
        assert_eq!(visible_legend_count(models, 5, 18, 28), 4);
        // …but at 32 it does, because `+7` needs only 4 cells after 5 items.
        assert_eq!(visible_legend_count(models, 5, 18, 32), 5);
    }

    #[test]
    fn legend_width_uses_rendered_character_width() {
        assert_eq!(visible_legend_count(["模型"], 1, 18, 5), 0);
        assert_eq!(visible_legend_count(["模型"], 1, 18, 6), 1);
    }

    #[test]
    fn legend_renders_square_markers_and_a_fully_visible_overflow_suffix() {
        let app = app_with_models(
            120,
            &[
                "model-alpha-000001",
                "model-alpha-000002",
                "model-alpha-000003",
                "model-alpha-000004",
            ],
        );
        // Exactly three 20-cell items plus gaps and the `+1` suffix fit.
        let legend_width = (3 * 20 + 2 * 2 + 2 + 2) as u16;
        let mut terminal = Terminal::new(TestBackend::new(legend_width, 1)).unwrap();

        terminal
            .draw(|frame| render_legend(frame, &app, frame.area()))
            .unwrap();

        let row = buffer_lines(&terminal).remove(0);
        let row = row.trim_end();
        assert_eq!(row.matches('■').count(), 3, "{row}");
        assert!(!row.contains('●'), "{row}");
        assert!(row.ends_with("+1"), "suffix must be fully visible: {row}");
    }

    #[test]
    fn legend_keeps_the_overflow_suffix_when_no_model_fits() {
        let app = app_with_models(120, &["一个名字非常非常长的模型"]);
        // Too narrow for even one truncated item: the legend must still show
        // the overflow count instead of going blank.
        let mut terminal = Terminal::new(TestBackend::new(10, 1)).unwrap();

        terminal
            .draw(|frame| render_legend(frame, &app, frame.area()))
            .unwrap();

        let row = buffer_lines(&terminal).remove(0);
        let row = row.trim_end();
        assert!(!row.contains('■'), "{row}");
        assert_eq!(row, "+1");
    }

    #[test]
    fn chart_box_renders_its_title_and_border_above_the_snapshot() {
        let width = 120;
        let height = 30;
        let mut app = make_app(width);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let state = crate::tui::view_state::ViewState::default();
        let presentation = crate::tui::presentation::Presentation::for_view(&app, &state);
        let actions = crate::tui::actions::ActionSet::for_view(&app, &state, presentation);

        terminal
            .draw(|frame| {
                crate::tui::ui::overview_snapshot::render(
                    frame,
                    &mut app,
                    frame.area(),
                    None,
                    &actions,
                )
            })
            .unwrap();

        let lines = buffer_lines(&terminal);
        let title_row = lines
            .iter()
            .position(|line| line.contains("Tokens per Day"))
            .expect("chart box title should render");
        let top_border = &lines[title_row];
        assert!(top_border.starts_with('┌'), "{top_border}");
        assert!(top_border.ends_with('┐'), "{top_border}");
        let snapshot_row = lines
            .iter()
            .position(|line| line.contains("Snapshot"))
            .expect("snapshot box should render below the chart box");
        assert!(title_row < snapshot_row);
        let bottom_border = &lines[snapshot_row - 1];
        assert!(
            bottom_border.starts_with('└') && bottom_border.ends_with('┘'),
            "{bottom_border}"
        );
    }
}
