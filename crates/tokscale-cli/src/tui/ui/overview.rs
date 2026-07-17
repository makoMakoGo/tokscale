use std::collections::BTreeMap;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::bar_chart::{render_stacked_bar_chart, ModelSegment, StackedBarData};
use crate::tui::app::{App, ChartGranularity};

const LEGEND_HORIZONTAL_PADDING: u16 = 2;
const LEGEND_ITEM_GAP: &str = "    ";
const LEGEND_MARKER: &str = "●";

#[derive(Debug, Clone, Default)]
struct ModelAggregate {
    provider: String,
    tokens: u64,
    cost: f64,
}

#[derive(Debug, Clone, Default)]
struct OverviewData {
    models: BTreeMap<String, ModelAggregate>,
}

pub(crate) fn render(frame: &mut Frame, app: &mut App, area: Rect) -> Rect {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme.background)),
        area,
    );

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
            Constraint::Length(chart_height.min(area.height)),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    render_chart(frame, app, chunks[0]);
    render_legend(frame, app, chunks[1]);
    chunks[2]
}

fn collect_overview_data(app: &App) -> OverviewData {
    let mut overview = OverviewData::default();

    for day in &app.data.daily {
        for source in day.source_breakdown.values() {
            for (model_key, model) in &source.models {
                let canonical =
                    canonical_model_key(model_key, &model.display_name, &model.color_key);
                let entry = overview.models.entry(canonical).or_default();
                if entry.provider.is_empty() && !model.provider.is_empty() {
                    entry.provider = model.provider.clone();
                }
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

fn canonical_model_key(model_key: &str, display_name: &str, color_key: &str) -> String {
    if !color_key.is_empty() {
        color_key.to_string()
    } else if !display_name.is_empty() {
        display_name.to_string()
    } else {
        model_key.to_string()
    }
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
                for source in day.source_breakdown.values() {
                    for (model_key, model) in &source.models {
                        let canonical =
                            canonical_model_key(model_key, &model.display_name, &model.color_key);
                        let entry = models.entry(canonical).or_default();
                        if entry.provider.is_empty() && !model.provider.is_empty() {
                            entry.provider = model.provider.clone();
                        }
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
                            color: app.model_color_for(&aggregate.provider, &model),
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
                for (model_key, model) in &hour.models {
                    let canonical =
                        canonical_model_key(model_key, &model.display_name, &model.color_key);
                    let entry = models.entry(canonical).or_default();
                    if entry.provider.is_empty() && !model.provider.is_empty() {
                        entry.provider = model.provider.clone();
                    }
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
                            color: app.model_color_for(&aggregate.provider, &model),
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
    // Match the Snapshot text inset: one cell for its border and one for padding.
    let area = area.inner(Margin {
        horizontal: LEGEND_HORIZONTAL_PADDING,
        vertical: 0,
    });
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
    let visible_count = visible_legend_count(
        models.iter().map(|(model, _)| model.as_str()),
        limit,
        name_width,
        area.width as usize,
    );
    let mut spans = Vec::new();
    for (index, (model, aggregate)) in models.into_iter().take(visible_count).enumerate() {
        if index > 0 {
            spans.push(Span::raw(LEGEND_ITEM_GAP));
        }
        spans.push(Span::styled(
            LEGEND_MARKER,
            Style::default().fg(app.model_color_for(&aggregate.provider, model)),
        ));
        spans.push(Span::raw(format!(
            " {}",
            truncate_string(model, name_width)
        )));
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
    let mut used_width = 0usize;
    let mut visible_count = 0;

    for model in models.into_iter().take(limit) {
        let display_name = truncate_string(model, name_width);
        let item_width = LEGEND_MARKER.width() + 1 + display_name.width();
        let gap_width = if visible_count == 0 {
            0
        } else {
            LEGEND_ITEM_GAP.width()
        };
        let required_width = gap_width + item_width;
        if used_width.saturating_add(required_width) > available_width {
            break;
        }

        used_width += required_width;
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

    #[test]
    fn canonical_model_prefers_color_key_over_grouped_label() {
        assert_eq!(
            canonical_model_key(
                "workspace-a / claude-sonnet-4",
                "workspace-a / claude-sonnet-4",
                "claude-sonnet-4",
            ),
            "claude-sonnet-4"
        );
    }

    #[test]
    fn legend_only_includes_complete_items_that_fit() {
        let models = ["123456789012345678"; 5];
        let three_items_width = 3 * 20 + 2 * LEGEND_ITEM_GAP.width();
        let fourth_item_width = LEGEND_ITEM_GAP.width() + 20;

        assert_eq!(visible_legend_count(models, 5, 18, three_items_width), 3);
        assert_eq!(
            visible_legend_count(models, 5, 18, three_items_width + fourth_item_width - 1),
            3
        );
        assert_eq!(
            visible_legend_count(models, 5, 18, three_items_width + fourth_item_width),
            4
        );
    }

    #[test]
    fn legend_width_uses_rendered_character_width() {
        assert_eq!(visible_legend_count(["模型"], 1, 18, 5), 0);
        assert_eq!(visible_legend_count(["模型"], 1, 18, 6), 1);
    }
}
