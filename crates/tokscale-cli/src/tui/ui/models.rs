use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
};

use super::widgets::{
    format_cache_hit_rate, format_cost, format_tokens, get_client_display_name,
    get_provider_display_name,
};
use crate::tui::app::{App, SortDirection, SortField};
use tokscale_core::GroupBy;

fn workspace_label(model: &crate::tui::data::ModelUsage) -> &str {
    model
        .workspace_label
        .as_deref()
        .unwrap_or("Unknown workspace")
}

fn model_display_name(model: &crate::tui::data::ModelUsage, group_by: &GroupBy) -> String {
    if *group_by == GroupBy::WorkspaceModel {
        format!("{} / {}", workspace_label(model), model.model)
    } else {
        model.model.clone()
    }
}

const TABLE_COLUMN_SPACING: u16 = 1;
const WIDE_MODEL_COLUMNS: u16 = 11;

const MODEL_MIN_WIDTH: u16 = 20;
const MODEL_MAX_WIDTH: u16 = 32;
const PROVIDER_MIN_WIDTH: u16 = 18;
const PROVIDER_MAX_WIDTH: u16 = 56;
const SOURCE_MIN_WIDTH: u16 = 18;
const SOURCE_MAX_WIDTH: u16 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextColumnSpec {
    min: u16,
    ideal: u16,
    weight: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextColumnWidths {
    model: u16,
    provider: u16,
    source: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelsTableLayout {
    widths: Vec<Constraint>,
    model_width: usize,
}

fn available_text_width(table_width: u16, fixed_width: u16, column_count: u16) -> u16 {
    let spacing = TABLE_COLUMN_SPACING.saturating_mul(column_count.saturating_sub(1));
    table_width
        .saturating_sub(fixed_width)
        .saturating_sub(spacing)
}

fn display_width(s: &str) -> u16 {
    s.chars().count().min(usize::from(u16::MAX)) as u16
}

fn clamped_content_width(content_width: u16, min: u16, max: u16) -> u16 {
    content_width.clamp(min, max)
}

fn distribute_remaining_width(widths: &mut [u16], specs: &[TextColumnSpec], mut remaining: u16) {
    while remaining > 0 {
        let mut advanced = false;

        for _ in 0..2 {
            for (index, spec) in specs.iter().enumerate() {
                if widths[index] >= spec.ideal {
                    continue;
                }

                let grow_by = remaining
                    .min(spec.weight)
                    .min(spec.ideal.saturating_sub(widths[index]));
                if grow_by == 0 {
                    continue;
                }

                widths[index] += grow_by;
                remaining -= grow_by;
                advanced = true;

                if remaining == 0 {
                    return;
                }
            }
        }

        if !advanced {
            return;
        }
    }
}

fn allocate_text_column_widths(budget: u16, specs: [TextColumnSpec; 3]) -> TextColumnWidths {
    let min_total = specs.iter().map(|spec| spec.min).sum::<u16>();

    if budget < min_total {
        let budget = u32::from(budget);
        let min_total = u32::from(min_total);
        let model = (budget * u32::from(specs[0].min) / min_total) as u16;
        let provider = (budget * u32::from(specs[1].min) / min_total) as u16;
        let source = (budget as u16)
            .saturating_sub(model)
            .saturating_sub(provider);

        return TextColumnWidths {
            model,
            provider,
            source,
        };
    }

    let mut widths = [specs[0].min, specs[1].min, specs[2].min];
    distribute_remaining_width(&mut widths, &specs, budget - min_total);

    TextColumnWidths {
        model: widths[0],
        provider: widths[1],
        source: widths[2],
    }
}

fn text_column_specs(
    model_content_width: u16,
    provider_content_width: u16,
    source_content_width: u16,
) -> [TextColumnSpec; 3] {
    [
        TextColumnSpec {
            min: MODEL_MIN_WIDTH,
            ideal: clamped_content_width(model_content_width, MODEL_MIN_WIDTH, MODEL_MAX_WIDTH),
            weight: 1,
        },
        TextColumnSpec {
            min: PROVIDER_MIN_WIDTH,
            ideal: clamped_content_width(
                provider_content_width,
                PROVIDER_MIN_WIDTH,
                PROVIDER_MAX_WIDTH,
            ),
            weight: 1,
        },
        TextColumnSpec {
            min: SOURCE_MIN_WIDTH,
            ideal: clamped_content_width(source_content_width, SOURCE_MIN_WIDTH, SOURCE_MAX_WIDTH),
            weight: 2,
        },
    ]
}

fn models_table_layout(
    table_width: u16,
    group_by: &GroupBy,
    is_narrow: bool,
    is_very_narrow: bool,
    model_content_width: u16,
    provider_content_width: u16,
    source_content_width: u16,
) -> ModelsTableLayout {
    if is_very_narrow {
        ModelsTableLayout {
            widths: vec![Constraint::Percentage(70), Constraint::Percentage(30)],
            model_width: 15,
        }
    } else if is_narrow {
        ModelsTableLayout {
            widths: vec![
                Constraint::Percentage(50),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ],
            model_width: 25,
        }
    } else if *group_by == GroupBy::WorkspaceModel {
        let text = allocate_text_column_widths(
            available_text_width(
                table_width,
                3 + 18 + 10 + 10 + 12 + 12 + 10 + 10,
                WIDE_MODEL_COLUMNS,
            ),
            text_column_specs(
                model_content_width,
                provider_content_width,
                source_content_width,
            ),
        );

        ModelsTableLayout {
            widths: vec![
                Constraint::Length(3),
                Constraint::Length(18),
                Constraint::Length(text.model),
                Constraint::Length(text.provider),
                Constraint::Length(text.source),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
            model_width: text.model as usize,
        }
    } else {
        let text = allocate_text_column_widths(
            available_text_width(
                table_width,
                3 + 10 + 10 + 10 + 10 + 8 + 10 + 10,
                WIDE_MODEL_COLUMNS,
            ),
            text_column_specs(
                model_content_width,
                provider_content_width,
                source_content_width,
            ),
        );

        ModelsTableLayout {
            widths: vec![
                Constraint::Length(3),
                Constraint::Length(text.model),
                Constraint::Length(text.provider),
                Constraint::Length(text.source),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
            model_width: text.model as usize,
        }
    }
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Models ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;
    app.set_max_visible_items(visible_height);

    let is_narrow = app.is_narrow();
    let is_very_narrow = app.is_very_narrow();
    let sort_field = app.sort_field;
    let sort_direction = app.sort_direction;
    let scroll_offset = app.scroll_offset;
    let selected_index = app.selected_index;
    let group_by = app.group_by.borrow().clone();
    let theme_accent = app.theme.accent;
    let theme_muted = app.theme.muted;
    let theme_selection = app.theme.selection;

    let models = app.get_sorted_models();
    if models.is_empty() {
        let empty_msg = Paragraph::new(
            "No usage data found. Press 'r' to refresh, 's' for sources, 'g' for grouping.",
        )
        .style(Style::default().fg(theme_muted))
        .alignment(Alignment::Center);
        frame.render_widget(empty_msg, inner);
        return;
    }

    let header_cells = if is_very_narrow {
        vec!["Model", "Cost"]
    } else if is_narrow {
        vec!["Model", "Tokens", "Cost"]
    } else if group_by == GroupBy::WorkspaceModel {
        vec![
            "#",
            "Workspace",
            "Model",
            "Provider",
            "Source",
            "Input",
            "Output",
            "Cache Read",
            "Cache Write",
            "Total",
            "Cost",
        ]
    } else {
        vec![
            "#", "Model", "Provider", "Source", "Input", "Output", "Cache R", "Cache W", "Cache×",
            "Total", "Cost",
        ]
    };

    let sort_indicator = |field: SortField| -> &'static str {
        if sort_field == field {
            match sort_direction {
                SortDirection::Ascending => " ▲",
                SortDirection::Descending => " ▼",
            }
        } else {
            ""
        }
    };

    let header = Row::new(
        header_cells
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let indicator = match i {
                    9 if !is_narrow => sort_indicator(SortField::Tokens),
                    10 if !is_narrow => sort_indicator(SortField::Cost),
                    1 if is_very_narrow => sort_indicator(SortField::Cost),
                    2 if is_narrow && !is_very_narrow => sort_indicator(SortField::Cost),
                    1 if is_narrow && !is_very_narrow => sort_indicator(SortField::Tokens),
                    _ => "",
                };
                Cell::from(format!("{}{}", h, indicator))
            })
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(theme_accent)
            .add_modifier(Modifier::BOLD),
    )
    .height(1);

    let models_len = models.len();
    let start = scroll_offset.min(models_len.saturating_sub(1));
    let end = (start + visible_height).min(models_len);

    if start >= models_len {
        return;
    }

    let visible_models = &models[start..end];
    let model_content_width = visible_models
        .iter()
        .map(|model| display_width(&model.model))
        .max()
        .unwrap_or(MODEL_MIN_WIDTH);
    let provider_content_width = visible_models
        .iter()
        .map(|model| display_width(&get_provider_display_name(&model.provider)))
        .max()
        .unwrap_or(PROVIDER_MIN_WIDTH);
    let source_content_width = visible_models
        .iter()
        .map(|model| display_width(&get_client_display_name(&model.client)))
        .max()
        .unwrap_or(SOURCE_MIN_WIDTH);
    let table_layout = models_table_layout(
        inner.width,
        &group_by,
        is_narrow,
        is_very_narrow,
        model_content_width,
        provider_content_width,
        source_content_width,
    );

    let rows: Vec<Row> = visible_models
        .iter()
        .enumerate()
        .map(|(i, model)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;

            let model_color = app.model_color_for(&model.provider, &model.model);
            let display_name = model_display_name(model, &group_by);

            let cells: Vec<Cell> = if is_very_narrow {
                vec![
                    Cell::from(truncate(&display_name, table_layout.model_width))
                        .style(Style::default().fg(model_color)),
                    Cell::from(format_cost(model.cost)).style(Style::default().fg(Color::Green)),
                ]
            } else if is_narrow {
                vec![
                    Cell::from(truncate(&display_name, table_layout.model_width))
                        .style(Style::default().fg(model_color)),
                    Cell::from(format_tokens(model.tokens.total())),
                    Cell::from(format_cost(model.cost)).style(Style::default().fg(Color::Green)),
                ]
            } else if group_by == GroupBy::WorkspaceModel {
                vec![
                    Cell::from(format!("{}", idx + 1)).style(Style::default().fg(theme_muted)),
                    Cell::from(truncate(workspace_label(model), 18)).style(
                        Style::default()
                            .fg(theme_accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Cell::from(truncate(&model.model, table_layout.model_width)).style(
                        Style::default()
                            .fg(model_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Cell::from(get_provider_display_name(&model.provider)),
                    Cell::from(get_client_display_name(&model.client))
                        .style(Style::default().fg(theme_muted)),
                    Cell::from(format_tokens(model.tokens.input))
                        .style(Style::default().fg(Color::Rgb(100, 200, 100))),
                    Cell::from(format_tokens(model.tokens.output))
                        .style(Style::default().fg(Color::Rgb(200, 100, 100))),
                    Cell::from(format_tokens(model.tokens.cache_read))
                        .style(Style::default().fg(Color::Rgb(100, 150, 200))),
                    Cell::from(format_tokens(model.tokens.cache_write))
                        .style(Style::default().fg(Color::Rgb(200, 150, 100))),
                    Cell::from(format_tokens(model.tokens.total())),
                    Cell::from(format_cost(model.cost)).style(Style::default().fg(Color::Green)),
                ]
            } else {
                vec![
                    Cell::from(format!("{}", idx + 1)).style(Style::default().fg(theme_muted)),
                    Cell::from(truncate(&model.model, table_layout.model_width)).style(
                        Style::default()
                            .fg(model_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Cell::from(get_provider_display_name(&model.provider)),
                    Cell::from(get_client_display_name(&model.client))
                        .style(Style::default().fg(theme_muted)),
                    Cell::from(format_tokens(model.tokens.input))
                        .style(Style::default().fg(Color::Rgb(100, 200, 100))),
                    Cell::from(format_tokens(model.tokens.output))
                        .style(Style::default().fg(Color::Rgb(200, 100, 100))),
                    Cell::from(format_tokens(model.tokens.cache_read))
                        .style(Style::default().fg(Color::Rgb(100, 150, 200))),
                    Cell::from(format_tokens(model.tokens.cache_write))
                        .style(Style::default().fg(Color::Rgb(200, 150, 100))),
                    Cell::from(format_cache_hit_rate(
                        model.tokens.cache_read,
                        model.tokens.input,
                        model.tokens.cache_write,
                    ))
                    .style(Style::default().fg(Color::Cyan)),
                    Cell::from(format_tokens(model.tokens.total())),
                    Cell::from(format_cost(model.cost)).style(Style::default().fg(Color::Green)),
                ]
            };

            let row_style = if is_selected {
                Style::default().bg(theme_selection)
            } else if is_striped {
                Style::default().bg(Color::Rgb(20, 24, 30))
            } else {
                Style::default()
            };

            Row::new(cells).style(row_style).height(1)
        })
        .collect();

    let widths = table_layout.widths;

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, inner);

    if models_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state = ScrollbarState::new(models_len).position(scroll_offset);

        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut scrollbar_state,
        );
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else if max_chars <= 3 {
        s.chars().take(max_chars).collect()
    } else {
        let head: String = s.chars().take(max_chars - 3).collect();
        format!("{}...", head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    fn model_layout(table_width: u16, model: u16, provider: u16, source: u16) -> ModelsTableLayout {
        models_table_layout(
            table_width,
            &GroupBy::Model,
            false,
            false,
            model,
            provider,
            source,
        )
    }

    fn workspace_model_layout(
        table_width: u16,
        model: u16,
        provider: u16,
        source: u16,
    ) -> ModelsTableLayout {
        models_table_layout(
            table_width,
            &GroupBy::WorkspaceModel,
            false,
            false,
            model,
            provider,
            source,
        )
    }

    #[test]
    fn wide_model_layout_shares_extra_width_with_provider_and_source() {
        let base = model_layout(140, 28, 42, 34);
        let wide = model_layout(180, 28, 42, 34);

        assert_eq!(length_at(&wide.widths, 1) as usize, wide.model_width);
        assert!(wide.model_width <= MODEL_MAX_WIDTH as usize);
        assert!(length_at(&wide.widths, 2) > length_at(&base.widths, 2));
        assert!(length_at(&wide.widths, 3) > length_at(&base.widths, 3));
        assert_eq!(length_at(&wide.widths, 3), 34);
    }

    #[test]
    fn wide_workspace_model_layout_shares_extra_width_with_provider_and_source() {
        let base = workspace_model_layout(160, 28, 42, 34);
        let wide = workspace_model_layout(200, 28, 42, 34);

        assert_eq!(length_at(&wide.widths, 2) as usize, wide.model_width);
        assert!(wide.model_width <= MODEL_MAX_WIDTH as usize);
        assert!(length_at(&wide.widths, 3) > length_at(&base.widths, 3));
        assert!(length_at(&wide.widths, 4) > length_at(&base.widths, 4));
        assert_eq!(length_at(&wide.widths, 4), 34);
    }

    #[test]
    fn model_column_stays_capped_on_very_wide_tables() {
        let layout = model_layout(260, 80, 120, 120);

        assert_eq!(length_at(&layout.widths, 1), MODEL_MAX_WIDTH);
        assert_eq!(layout.model_width, MODEL_MAX_WIDTH as usize);
        assert_eq!(length_at(&layout.widths, 2), PROVIDER_MAX_WIDTH);
        assert_eq!(length_at(&layout.widths, 3), SOURCE_MAX_WIDTH);
    }

    #[test]
    fn source_column_stops_growing_after_visible_content_fits() {
        let fit = model_layout(180, 28, 56, 26);
        let wider = model_layout(220, 28, 56, 26);

        assert_eq!(length_at(&fit.widths, 3), 26);
        assert_eq!(length_at(&wider.widths, 3), 26);
        assert!(length_at(&wider.widths, 2) > length_at(&fit.widths, 2));
    }

    #[test]
    fn leftover_width_is_not_forced_into_text_columns_after_content_fits() {
        let fit = model_layout(180, 28, 32, 26);
        let wider = model_layout(260, 28, 32, 26);

        assert_eq!(length_at(&wider.widths, 1), length_at(&fit.widths, 1));
        assert_eq!(length_at(&wider.widths, 2), length_at(&fit.widths, 2));
        assert_eq!(length_at(&wider.widths, 3), length_at(&fit.widths, 3));
    }
}
