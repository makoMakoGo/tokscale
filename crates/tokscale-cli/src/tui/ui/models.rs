use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
};

use super::model_usage_layout::{
    display_width, model_usage_table_layout, ModelUsageColumn as ModelsColumn,
    ModelUsageTableDensity as ModelsTableDensity, ModelUsageTableLayout as ModelsTableLayout,
    DETAIL_PROVIDER_WIDTH, DETAIL_SOURCE_WIDTH, MODEL_MIN_WIDTH,
};
use super::widgets::{
    format_cache_hit_rate, format_cost, format_ms_per_1k, format_tokens, get_client_display_name,
    get_provider_display_name, truncate_model_display_name_to,
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

fn models_table_layout(
    table_width: u16,
    is_very_narrow: bool,
    model_content_width: u16,
    provider_content_width: u16,
    source_content_width: u16,
) -> ModelsTableLayout {
    model_usage_table_layout(
        table_width,
        is_very_narrow,
        model_content_width,
        provider_content_width,
        source_content_width,
        &[
            ModelsColumn::Source,
            ModelsColumn::Provider,
            ModelsColumn::Input,
            ModelsColumn::Output,
            ModelsColumn::CacheRate,
            ModelsColumn::CacheRead,
            ModelsColumn::CacheWrite,
            ModelsColumn::Performance,
        ],
    )
}

fn model_column_header(
    column: ModelsColumn,
    group_by: &GroupBy,
    density: ModelsTableDensity,
) -> &'static str {
    match column {
        ModelsColumn::Model => "Model",
        ModelsColumn::Messages => "Msgs",
        ModelsColumn::Provider => "Provider",
        ModelsColumn::Source => "Source",
        ModelsColumn::Input => "Input",
        ModelsColumn::Output => "Output",
        ModelsColumn::CacheRead if *group_by == GroupBy::WorkspaceModel => "Cache Read",
        ModelsColumn::CacheRead => "Cache R",
        ModelsColumn::CacheWrite if *group_by == GroupBy::WorkspaceModel => "Cache Write",
        ModelsColumn::CacheWrite => "Cache W",
        ModelsColumn::CacheRate => "Cache×",
        ModelsColumn::Total if density == ModelsTableDensity::Full => "Total",
        ModelsColumn::Total => "Tokens",
        ModelsColumn::Performance => "ms/1K",
        ModelsColumn::Cost => "Cost",
    }
}

fn model_column_sort_field(column: ModelsColumn) -> Option<SortField> {
    match column {
        ModelsColumn::Total => Some(SortField::Tokens),
        ModelsColumn::Cost => Some(SortField::Cost),
        _ => None,
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

    let models_len = models.len();
    let start = scroll_offset.min(models_len.saturating_sub(1));
    let end = (start + visible_height).min(models_len);

    if start >= models_len {
        return;
    }

    let model_content_width = models
        .iter()
        .map(|model| display_width(&model.model))
        .max()
        .unwrap_or(MODEL_MIN_WIDTH);
    let provider_content_width = models
        .iter()
        .map(|model| display_width(&get_provider_display_name(&model.provider)))
        .max()
        .unwrap_or(DETAIL_PROVIDER_WIDTH);
    let source_content_width = models
        .iter()
        .map(|model| display_width(&get_client_display_name(&model.client)))
        .max()
        .unwrap_or(DETAIL_SOURCE_WIDTH);
    let visible_models = &models[start..end];
    let table_layout = models_table_layout(
        inner.width,
        is_very_narrow,
        model_content_width,
        provider_content_width,
        source_content_width,
    );
    let columns = table_layout.columns.clone();
    let header = Row::new(
        columns
            .iter()
            .map(|column| {
                let h = model_column_header(*column, &group_by, table_layout.density);
                let indicator = model_column_sort_field(*column)
                    .map(sort_indicator)
                    .unwrap_or("");
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

    let rows: Vec<Row> = visible_models
        .iter()
        .enumerate()
        .map(|(i, model)| {
            let idx = i + start;
            let is_selected = idx == selected_index;
            let is_striped = idx % 2 == 1;

            let model_color = app.model_color_for(&model.provider, &model.model);
            let display_name = model_display_name(model, &group_by);
            let cell_for_column = |column: ModelsColumn| -> Cell {
                match column {
                    ModelsColumn::Model => Cell::from(truncate_model_display_name_to(
                        &display_name,
                        table_layout.model_width,
                    ))
                    .style(
                        Style::default()
                            .fg(model_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    ModelsColumn::Provider => {
                        Cell::from(get_provider_display_name(&model.provider))
                    }
                    // models_table_layout never includes Messages; panic if renderer and layout diverge.
                    ModelsColumn::Messages => unreachable!("models rows do not have message data"),
                    ModelsColumn::Source => Cell::from(get_client_display_name(&model.client))
                        .style(Style::default().fg(theme_muted)),
                    ModelsColumn::Input => Cell::from(format_tokens(model.tokens.input))
                        .style(Style::default().fg(Color::Rgb(100, 200, 100))),
                    ModelsColumn::Output => Cell::from(format_tokens(model.tokens.output))
                        .style(Style::default().fg(Color::Rgb(200, 100, 100))),
                    ModelsColumn::CacheRead => Cell::from(format_tokens(model.tokens.cache_read))
                        .style(Style::default().fg(Color::Rgb(100, 150, 200))),
                    ModelsColumn::CacheWrite => Cell::from(format_tokens(model.tokens.cache_write))
                        .style(Style::default().fg(Color::Rgb(200, 150, 100))),
                    ModelsColumn::CacheRate => Cell::from(format_cache_hit_rate(
                        model.tokens.cache_read,
                        model.tokens.input,
                        model.tokens.cache_write,
                    ))
                    .style(Style::default().fg(Color::Cyan)),
                    ModelsColumn::Total => Cell::from(format_tokens(model.tokens.total())),
                    ModelsColumn::Performance => {
                        Cell::from(format_ms_per_1k(model.performance.ms_per_1k_tokens))
                            .style(Style::default().fg(Color::Yellow))
                    }
                    ModelsColumn::Cost => {
                        Cell::from(format_cost(model.cost)).style(Style::default().fg(Color::Green))
                    }
                }
            };
            let cells: Vec<Cell> = columns
                .iter()
                .map(|column| cell_for_column(*column))
                .collect();

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

#[cfg(test)]
mod tests {
    use super::super::model_usage_layout::{MODEL_MAX_WIDTH, PROVIDER_MAX_WIDTH, SOURCE_MAX_WIDTH};
    use super::*;

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    fn model_layout(table_width: u16, model: u16, provider: u16, source: u16) -> ModelsTableLayout {
        models_table_layout(table_width, false, model, provider, source)
    }

    fn workspace_model_layout(
        table_width: u16,
        model: u16,
        provider: u16,
        source: u16,
    ) -> ModelsTableLayout {
        models_table_layout(table_width, false, model, provider, source)
    }

    #[test]
    fn portrait_model_layout_keeps_core_columns_before_cache_details() {
        let layout = model_layout(100, 28, 42, 34);

        assert_eq!(layout.density, ModelsTableDensity::Detail);
        assert_eq!(
            layout.columns,
            vec![
                ModelsColumn::Model,
                ModelsColumn::Source,
                ModelsColumn::Provider,
                ModelsColumn::Input,
                ModelsColumn::Output,
                ModelsColumn::CacheRate,
                ModelsColumn::Total,
                ModelsColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&ModelsColumn::CacheRead));
        assert!(!layout.columns.contains(&ModelsColumn::CacheWrite));
        assert!(layout.model_width >= MODEL_MIN_WIDTH as usize);
    }

    #[test]
    fn narrow_model_layout_keeps_model_tokens_and_cost() {
        let layout = models_table_layout(74, false, 80, 56, 40);

        assert_eq!(
            layout.columns,
            vec![
                ModelsColumn::Model,
                ModelsColumn::Source,
                ModelsColumn::Provider,
                ModelsColumn::Total,
                ModelsColumn::Cost,
            ]
        );
        assert_eq!(layout.model_width, MODEL_MAX_WIDTH as usize);
    }

    #[test]
    fn very_narrow_model_layout_still_keeps_tokens_before_cache_details() {
        let layout = models_table_layout(54, true, 80, 56, 40);

        assert_eq!(layout.density, ModelsTableDensity::VeryCompact);
        assert_eq!(
            layout.columns,
            vec![ModelsColumn::Model, ModelsColumn::Total, ModelsColumn::Cost]
        );
        assert!(!layout.columns.contains(&ModelsColumn::CacheRead));
    }

    #[test]
    fn wide_model_layout_is_required_before_cache_columns_are_shown() {
        let portrait = model_layout(100, 28, 42, 34);
        let wide = model_layout(140, 28, 42, 34);

        assert_eq!(portrait.density, ModelsTableDensity::Detail);
        assert_eq!(wide.density, ModelsTableDensity::Full);
        assert!(wide.columns.contains(&ModelsColumn::CacheRead));
        assert!(wide.columns.contains(&ModelsColumn::CacheWrite));
        assert!(wide.columns.contains(&ModelsColumn::CacheRate));
    }

    #[test]
    fn wide_model_layout_shares_extra_width_with_provider_and_source() {
        let base = model_layout(140, 28, 42, 34);
        let wide = model_layout(180, 28, 42, 34);

        assert_eq!(length_at(&wide.widths, 0) as usize, wide.model_width);
        assert!(wide.model_width <= MODEL_MAX_WIDTH as usize);
        assert!(wide.columns.contains(&ModelsColumn::Source));
        assert!(wide.columns.contains(&ModelsColumn::Provider));
        assert!(length_at(&wide.widths, 2) > length_at(&base.widths, 2));
        assert_eq!(length_at(&wide.widths, 1), 34);
    }

    #[test]
    fn wide_workspace_model_layout_shares_extra_width_with_provider_and_source() {
        let base = workspace_model_layout(160, 28, 42, 34);
        let wide = workspace_model_layout(200, 28, 42, 34);

        assert_eq!(length_at(&wide.widths, 0) as usize, wide.model_width);
        assert!(wide.model_width <= MODEL_MAX_WIDTH as usize);
        assert!(wide.columns.contains(&ModelsColumn::Source));
        assert!(wide.columns.contains(&ModelsColumn::Provider));
        assert!(length_at(&wide.widths, 2) > length_at(&base.widths, 2));
        assert_eq!(length_at(&wide.widths, 1), 34);
    }

    #[test]
    fn display_width_uses_terminal_columns_for_unicode() {
        assert_eq!(display_width("模型"), 4);
        assert_eq!(display_width("e\u{301}"), 1);
    }

    #[test]
    fn truncate_uses_terminal_columns_for_unicode() {
        assert_eq!(truncate_model_display_name_to("模型abc", 5), "模...");
        assert_eq!(truncate_model_display_name_to("模型abc", 7), "模型abc");
    }

    #[test]
    fn full_model_list_controls_layout_width_not_visible_page() {
        let visible_only = model_layout(180, 28, 28, 26);
        let full_dataset = model_layout(180, 28, 56, 26);

        assert_eq!(visible_only.columns, full_dataset.columns);
        assert!(length_at(&full_dataset.widths, 2) > length_at(&visible_only.widths, 2));
    }

    #[test]
    fn model_column_stays_capped_on_very_wide_tables() {
        let layout = model_layout(260, 80, 120, 120);

        assert_eq!(length_at(&layout.widths, 0), MODEL_MAX_WIDTH);
        assert_eq!(layout.model_width, MODEL_MAX_WIDTH as usize);
        assert_eq!(length_at(&layout.widths, 2), PROVIDER_MAX_WIDTH);
        assert_eq!(length_at(&layout.widths, 1), SOURCE_MAX_WIDTH);
    }

    #[test]
    fn source_column_stops_growing_after_visible_content_fits() {
        let fit = model_layout(220, 28, 56, 26);
        let wider = model_layout(260, 28, 56, 26);

        assert_eq!(length_at(&fit.widths, 1), 26);
        assert_eq!(length_at(&wider.widths, 1), 26);
        assert_eq!(length_at(&wider.widths, 2), length_at(&fit.widths, 2));
    }

    #[test]
    fn leftover_width_is_not_forced_into_text_columns_after_content_fits() {
        let fit = model_layout(180, 28, 32, 26);
        let wider = model_layout(260, 28, 32, 26);

        assert_eq!(length_at(&wider.widths, 0), length_at(&fit.widths, 0));
        assert_eq!(length_at(&wider.widths, 1), length_at(&fit.widths, 1));
        assert_eq!(length_at(&wider.widths, 2), length_at(&fit.widths, 2));
    }
}
