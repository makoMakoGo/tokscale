use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Scrollbar, ScrollbarOrientation, Table};

use super::empty_state;
use super::model_usage_layout::{
    model_usage_table_layout, ModelUsageColumn as ModelsColumn, ModelUsageLayoutSchema,
    ModelUsageTableDensity as ModelsTableDensity, ModelUsageTableLayout as ModelsTableLayout,
    DETAIL_CLIENT_WIDTH, DETAIL_PROVIDER_WIDTH, MODEL_MIN_WIDTH, WORKSPACE_MIN_WIDTH,
};
use super::table_layout::{
    display_width, distributed_table_area, DISTRIBUTED_TABLE_FLEX, TABLE_COLUMN_SPACING,
};
use super::widgets::{
    format_cache_hit_rate, format_cost, format_cost_per_million, format_ms_per_1k, format_tokens,
    get_client_display_name, get_provider_display_name, total_tokens_cell, truncate_display_width,
    truncate_model_display_name_to, viewport_scrollbar_state, workspace_label_or_unknown,
};
use crate::tui::actions::ActionSet;
use crate::tui::app::{App, ModelDetailSelection, SortDirection, SortField};
use crate::tui::presentation::EmptySubject;
use tokscale_core::GroupBy;

fn workspace_label(model: &crate::tui::data::ModelUsage) -> &str {
    workspace_label_or_unknown(
        model
            .workspace_label
            .as_deref()
            .or(model.workspace_key.as_deref()),
    )
}

/// The Model column always shows the bare canonical model; under
/// `GroupBy::WorkspaceModel` the workspace dimension lives in its own column
/// instead of a "workspace / model" prefix (ADR 0026).
fn model_display_name(model: &crate::tui::data::ModelUsage) -> &str {
    &model.model
}

fn model_content_width(models: &[&crate::tui::data::ModelUsage]) -> u16 {
    models
        .iter()
        .map(|model| display_width(model_display_name(model)))
        .max()
        .unwrap_or(MODEL_MIN_WIDTH)
}

fn workspace_content_width(models: &[&crate::tui::data::ModelUsage]) -> u16 {
    models
        .iter()
        .map(|model| display_width(workspace_label(model)))
        .max()
        .unwrap_or(WORKSPACE_MIN_WIDTH)
}

fn models_table_layout(
    table_width: u16,
    model_content_width: u16,
    provider_content_width: u16,
    client_content_width: u16,
    workspace_content_width: u16,
    group_by: &GroupBy,
    detail: Option<&ModelDetailSelection>,
) -> ModelsTableLayout {
    let schema = match detail {
        Some(selection) if selection.client.is_some() => {
            ModelUsageLayoutSchema::ClientModelProviderDetails
        }
        Some(_) => ModelUsageLayoutSchema::ModelProviderDetails,
        None if *group_by == GroupBy::WorkspaceModel => ModelUsageLayoutSchema::WorkspaceModels,
        None => ModelUsageLayoutSchema::Models,
    };

    model_usage_table_layout(
        table_width,
        model_content_width,
        provider_content_width,
        client_content_width,
        workspace_content_width,
        schema,
    )
}

fn model_column_header(
    column: ModelsColumn,
    group_by: &GroupBy,
    density: ModelsTableDensity,
) -> &'static str {
    match column {
        ModelsColumn::Workspace => "Workspace",
        ModelsColumn::Model => "Model",
        ModelsColumn::Messages => "Msgs",
        ModelsColumn::Provider => "Provider",
        ModelsColumn::Client => "Client",
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
        ModelsColumn::CostPerMillion => "Cost/1M",
    }
}

fn model_column_sort_field(column: ModelsColumn) -> Option<SortField> {
    match column {
        ModelsColumn::Total => Some(SortField::Tokens),
        ModelsColumn::Cost => Some(SortField::Cost),
        ModelsColumn::CostPerMillion => None,
        _ => None,
    }
}

pub fn render(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    empty: Option<EmptySubject>,
    actions: &ActionSet,
) {
    let title = match &app.selected_model_detail {
        Some(selection) => match selection.client.as_deref() {
            Some(client) => format!(
                " Model Details · {} · {} ",
                get_client_display_name(client),
                selection.model
            ),
            None => format!(" Model Details · {} ", selection.model),
        },
        None => " Models ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            title,
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    let table_area = distributed_table_area(inner);
    frame.render_widget(block, area);

    let visible_height = inner.height.saturating_sub(1) as usize;
    app.set_max_visible_items(visible_height);
    if empty_state::render_if(frame, app, inner, empty, actions) {
        return;
    }

    let sort_field = app.sort_field;
    let sort_direction = app.sort_direction;
    let scroll_offset = app.scroll_offset;
    let selected_index = app.selected_index;
    let group_by = app.group_by.borrow().clone();
    let theme_accent = app.theme.accent;
    let theme_muted = app.theme.muted;
    let theme_selection = app.theme.selection;
    let metric_input_style = app.theme.metric_input_style();
    let metric_output_style = app.theme.metric_output_style();
    let metric_cache_read_style = app.theme.metric_cache_read_style();
    let metric_cache_write_style = app.theme.metric_cache_write_style();
    let striped_row_style = app.theme.striped_row_style();

    let models = app.get_sorted_models();

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

    let model_content_width = model_content_width(&models);
    let provider_content_width = models
        .iter()
        .map(|model| display_width(&get_provider_display_name(&model.provider)))
        .max()
        .unwrap_or(DETAIL_PROVIDER_WIDTH);
    let client_content_width = models
        .iter()
        .map(|model| display_width(&get_client_display_name(&model.client)))
        .max()
        .unwrap_or(DETAIL_CLIENT_WIDTH);
    let workspace_content_width = if group_by == GroupBy::WorkspaceModel {
        workspace_content_width(&models)
    } else {
        0
    };
    let visible_models = &models[start..end];
    let table_layout = models_table_layout(
        table_area.width,
        model_content_width,
        provider_content_width,
        client_content_width,
        workspace_content_width,
        &group_by,
        app.selected_model_detail.as_ref(),
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

            let model_color = app.model_color(&model.model);
            let display_name = model_display_name(model);
            let cell_for_column = |column: ModelsColumn| -> Cell {
                match column {
                    ModelsColumn::Workspace => Cell::from(truncate_display_width(
                        workspace_label(model),
                        table_layout.width_for(ModelsColumn::Workspace),
                    ))
                    .style(Style::default().fg(theme_muted)),
                    ModelsColumn::Model => Cell::from(truncate_model_display_name_to(
                        display_name,
                        table_layout.model_width,
                    ))
                    .style(
                        Style::default()
                            .fg(model_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    ModelsColumn::Provider => Cell::from(truncate_display_width(
                        &get_provider_display_name(&model.provider),
                        table_layout.width_for(ModelsColumn::Provider),
                    )),
                    // models_table_layout never includes Messages; panic if renderer and layout diverge.
                    ModelsColumn::Messages => unreachable!("models rows do not have message data"),
                    ModelsColumn::Client => Cell::from(truncate_display_width(
                        &get_client_display_name(&model.client),
                        table_layout.width_for(ModelsColumn::Client),
                    ))
                    .style(Style::default().fg(theme_muted)),
                    ModelsColumn::Input => {
                        Cell::from(format_tokens(model.tokens.input)).style(metric_input_style)
                    }
                    ModelsColumn::Output => {
                        Cell::from(format_tokens(model.tokens.displayed_output()))
                            .style(metric_output_style)
                    }
                    ModelsColumn::CacheRead => Cell::from(format_tokens(model.tokens.cache_read))
                        .style(metric_cache_read_style),
                    ModelsColumn::CacheWrite => Cell::from(format_tokens(model.tokens.cache_write))
                        .style(metric_cache_write_style),
                    ModelsColumn::CacheRate => Cell::from(format_cache_hit_rate(
                        model.tokens.cache_read,
                        model.tokens.input,
                        model.tokens.cache_write,
                    ))
                    .style(Style::default().fg(Color::Cyan)),
                    ModelsColumn::Total => total_tokens_cell(model.tokens.total(), &app.theme),
                    ModelsColumn::Performance => {
                        Cell::from(format_ms_per_1k(model.performance.ms_per_1k_tokens))
                            .style(Style::default().fg(Color::Yellow))
                    }
                    ModelsColumn::Cost => {
                        Cell::from(format_cost(model.cost)).style(Style::default().fg(Color::Green))
                    }
                    ModelsColumn::CostPerMillion => {
                        Cell::from(format_cost_per_million(model.cost, model.tokens.total()))
                            .style(Style::default().fg(Color::Rgb(150, 200, 150)))
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
                striped_row_style
            } else {
                Style::default()
            };

            Row::new(cells).style(row_style).height(1)
        })
        .collect();

    let widths = table_layout.widths;

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(TABLE_COLUMN_SPACING)
        .flex(DISTRIBUTED_TABLE_FLEX)
        .row_highlight_style(Style::default().bg(theme_selection));

    frame.render_widget(table, table_area);

    if models_len > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));

        let mut scrollbar_state =
            viewport_scrollbar_state(models_len, scroll_offset, visible_height);

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
    use super::super::model_usage_layout::{
        CLIENT_MAX_WIDTH, MODEL_MAX_WIDTH, PROVIDER_MAX_WIDTH, WORKSPACE_MAX_WIDTH,
    };
    use super::*;

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    fn model_layout(table_width: u16, model: u16, provider: u16, client: u16) -> ModelsTableLayout {
        models_table_layout(
            table_width,
            model,
            provider,
            client,
            0,
            &GroupBy::Model,
            None,
        )
    }

    fn workspace_model_layout(
        table_width: u16,
        model: u16,
        provider: u16,
        client: u16,
    ) -> ModelsTableLayout {
        models_table_layout(
            table_width,
            model,
            provider,
            client,
            22,
            &GroupBy::WorkspaceModel,
            None,
        )
    }

    #[test]
    fn portrait_model_layout_stops_at_first_non_fitting_priority_column() {
        let layout = model_layout(100, 28, 42, 34);

        assert_eq!(layout.density, ModelsTableDensity::Detail);
        assert_eq!(
            layout.columns,
            vec![
                ModelsColumn::Model,
                ModelsColumn::Client,
                ModelsColumn::Total,
                ModelsColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&ModelsColumn::Provider));
        assert!(!layout.columns.contains(&ModelsColumn::Input));
        assert!(!layout.columns.contains(&ModelsColumn::Output));
        assert_eq!(layout.model_width, 28);
    }

    #[test]
    fn narrow_model_layout_stops_before_context_columns_before_truncating_model() {
        let layout = models_table_layout(74, 80, 56, 40, 0, &GroupBy::Model, None);

        assert_eq!(
            layout.columns,
            vec![ModelsColumn::Model, ModelsColumn::Total, ModelsColumn::Cost]
        );
        assert_eq!(layout.model_width, 29);
        assert!(!layout.columns.contains(&ModelsColumn::Client));
        assert!(!layout.columns.contains(&ModelsColumn::Provider));
        assert!(!layout.columns.contains(&ModelsColumn::Input));
    }

    #[test]
    fn very_narrow_model_layout_keeps_tokens_before_optional_detail_columns() {
        let layout = models_table_layout(54, 80, 56, 40, 0, &GroupBy::Model, None);

        assert_eq!(layout.density, ModelsTableDensity::Core);
        assert_eq!(
            layout.columns,
            vec![ModelsColumn::Model, ModelsColumn::Total, ModelsColumn::Cost,]
        );
        assert_eq!(layout.model_width, 29);
        assert!(!layout.columns.contains(&ModelsColumn::Input));
    }

    #[test]
    fn model_detail_layout_omits_dimensions_locked_in_the_title() {
        let model_detail = ModelDetailSelection {
            model: "shared-model".to_string(),
            client: None,
        };
        let client_model_detail = ModelDetailSelection {
            model: "shared-model".to_string(),
            client: Some("claude".to_string()),
        };

        let by_model =
            models_table_layout(180, 80, 56, 40, 0, &GroupBy::Model, Some(&model_detail));
        let by_client_model = models_table_layout(
            180,
            80,
            56,
            40,
            0,
            &GroupBy::ClientModel,
            Some(&client_model_detail),
        );

        assert!(!by_model.columns.contains(&ModelsColumn::Model));
        assert!(by_model.columns.contains(&ModelsColumn::Client));
        assert!(by_model.columns.contains(&ModelsColumn::Provider));
        assert!(!by_client_model.columns.contains(&ModelsColumn::Model));
        assert!(!by_client_model.columns.contains(&ModelsColumn::Client));
        assert!(by_client_model.columns.contains(&ModelsColumn::Provider));
    }

    #[test]
    fn wider_model_layout_keeps_cache_columns_when_min_widths_fit() {
        let portrait = model_layout(100, 28, 42, 34);
        let wide = model_layout(180, 28, 42, 34);

        assert_eq!(portrait.density, ModelsTableDensity::Detail);
        assert_eq!(wide.density, ModelsTableDensity::Full);
        assert!(wide.columns.contains(&ModelsColumn::CacheRead));
        assert!(wide.columns.contains(&ModelsColumn::CacheWrite));
        assert!(wide.columns.contains(&ModelsColumn::CacheRate));
    }

    #[test]
    fn wide_model_layout_keeps_provider_and_client_content_widths() {
        let base = model_layout(140, 28, 42, 34);
        let wide = model_layout(180, 28, 42, 34);

        assert_eq!(length_at(&wide.widths, 0) as usize, wide.model_width);
        assert!(wide.model_width <= MODEL_MAX_WIDTH as usize);
        assert!(wide.columns.contains(&ModelsColumn::Client));
        assert!(wide.columns.contains(&ModelsColumn::Provider));
        assert_eq!(length_at(&base.widths, 1), 34);
        assert_eq!(length_at(&base.widths, 2), PROVIDER_MAX_WIDTH);
        assert_eq!(length_at(&wide.widths, 1), 34);
        assert_eq!(length_at(&wide.widths, 2), PROVIDER_MAX_WIDTH);
    }

    #[test]
    fn wide_workspace_model_layout_keeps_provider_and_client_content_widths() {
        let base = workspace_model_layout(160, 28, 42, 34);
        let wide = workspace_model_layout(200, 28, 42, 34);

        assert_eq!(length_at(&wide.widths, 1) as usize, wide.model_width);
        assert!(wide.model_width <= MODEL_MAX_WIDTH as usize);
        assert!(wide.columns.contains(&ModelsColumn::Client));
        assert!(wide.columns.contains(&ModelsColumn::Provider));
        assert_eq!(length_at(&base.widths, 2), 34);
        assert_eq!(length_at(&base.widths, 3), PROVIDER_MAX_WIDTH);
        assert_eq!(length_at(&wide.widths, 2), 34);
        assert_eq!(length_at(&wide.widths, 3), PROVIDER_MAX_WIDTH);
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
        let visible_only = model_layout(140, 28, 12, 12);
        let full_dataset = model_layout(140, 28, 22, 12);

        assert!(length_at(&full_dataset.widths, 2) > length_at(&visible_only.widths, 2));
    }

    #[test]
    fn model_column_stays_capped_on_very_wide_tables() {
        let layout = model_layout(400, 80, 120, 120);

        assert_eq!(length_at(&layout.widths, 0), MODEL_MAX_WIDTH);
        assert_eq!(layout.model_width, MODEL_MAX_WIDTH as usize);
        assert_eq!(length_at(&layout.widths, 1), CLIENT_MAX_WIDTH);
        assert_eq!(length_at(&layout.widths, 2), PROVIDER_MAX_WIDTH);
    }

    #[test]
    fn workspace_and_model_columns_stay_capped_on_very_wide_tables() {
        let layout = workspace_model_layout(400, 80, 120, 120);

        assert_eq!(length_at(&layout.widths, 0), WORKSPACE_MAX_WIDTH);
        assert_eq!(length_at(&layout.widths, 1), MODEL_MAX_WIDTH);
        assert_eq!(layout.model_width, MODEL_MAX_WIDTH as usize);
    }

    #[test]
    fn client_column_stays_at_content_width_until_cap() {
        let fit = model_layout(220, 28, 56, 26);
        let wider = model_layout(260, 28, 56, 26);

        assert_eq!(length_at(&fit.widths, 1), 26);
        assert_eq!(length_at(&wider.widths, 1), length_at(&fit.widths, 1));
        assert_eq!(length_at(&wider.widths, 2), length_at(&fit.widths, 2));
    }

    #[test]
    fn workspace_model_widths_split_workspace_from_bare_model() {
        let model = crate::tui::data::ModelUsage {
            model: "gpt-5".to_string(),
            provider: "openai".to_string(),
            client: "opencode".to_string(),
            workspace_key: Some("/work/project".to_string()),
            workspace_label: Some("project-with-long-name".to_string()),
            tokens: crate::tui::data::TokenBreakdown {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: 0.0,
            performance: Default::default(),
            session_count: 0,
        };
        let models = vec![&model];

        assert_eq!(model_content_width(&models), 5);
        assert_eq!(
            workspace_content_width(&models),
            display_width("project-with-long-name")
        );
        assert_eq!(model_display_name(&model), "gpt-5");
    }

    #[test]
    fn workspace_column_falls_back_to_key_when_label_missing() {
        let model = crate::tui::data::ModelUsage {
            model: "gpt-5".to_string(),
            provider: "openai".to_string(),
            client: "opencode".to_string(),
            workspace_key: Some("/work/project".to_string()),
            workspace_label: None,
            tokens: crate::tui::data::TokenBreakdown::default(),
            cost: 0.0,
            performance: Default::default(),
            session_count: 0,
        };

        assert_eq!(workspace_label(&model), "/work/project");
    }

    #[test]
    fn leftover_width_does_not_expand_content_columns() {
        let fit = model_layout(180, 28, 32, 26);
        let wider = model_layout(260, 28, 32, 26);

        assert_eq!(wider.columns, fit.columns);
        assert_eq!(
            (0..wider.widths.len())
                .map(|index| length_at(&wider.widths, index))
                .collect::<Vec<_>>(),
            (0..fit.widths.len())
                .map(|index| length_at(&fit.widths, index))
                .collect::<Vec<_>>()
        );
    }

    use crate::tui::app::Tab;
    use crate::tui::app::TuiConfig;
    use ratatui::{backend::TestBackend, Terminal};

    fn make_models_app(width: u16, group_by: GroupBy) -> App {
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
        app.current_tab = Tab::Models;
        *app.group_by.borrow_mut() = group_by;
        app
    }

    fn make_model_detail_app(group_by: GroupBy) -> App {
        let messages = [
            tokscale_core::UnifiedMessage::new(
                "claude",
                "shared-model",
                "anthropic",
                "anthropic-session",
                1_800_000_000,
                tokscale_core::TokenBreakdown {
                    input: 10,
                    ..Default::default()
                },
                0.1,
            ),
            tokscale_core::UnifiedMessage::new(
                "claude",
                "shared-model",
                "openrouter",
                "openrouter-session",
                1_800_000_001,
                tokscale_core::TokenBreakdown {
                    input: 20,
                    ..Default::default()
                },
                0.2,
            ),
        ];
        let accumulator =
            tokscale_core::build_tui_accumulator(&messages, tokscale_core::DateRange::none());
        let mut app = make_models_app(180, group_by.clone());
        app.data = accumulator.project(&group_by);
        app.data_group_by = group_by;
        app.projection_backend = Some(crate::tui::app::ProjectionBackend::Memory(accumulator));
        app
    }

    fn workspace_model_usage(
        model: &str,
        workspace: &str,
        cost: f64,
    ) -> crate::tui::data::ModelUsage {
        crate::tui::data::ModelUsage {
            model: model.to_string(),
            provider: "openai".to_string(),
            client: "opencode".to_string(),
            workspace_key: Some(format!("/work/{workspace}")),
            workspace_label: Some(workspace.to_string()),
            tokens: crate::tui::data::TokenBreakdown::default(),
            cost,
            performance: Default::default(),
            session_count: 1,
        }
    }

    fn render_body(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = crate::tui::view_state::ViewState::default();
        let presentation = crate::tui::presentation::Presentation::for_view(app, &state);
        let actions = ActionSet::for_view(app, &state, presentation);
        terminal
            .draw(|frame| render(frame, app, Rect::new(0, 0, width, height), None, &actions))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn models_table_shows_workspace_column_under_workspace_grouping() {
        let mut app = make_models_app(140, GroupBy::WorkspaceModel);
        app.data.models = vec![
            workspace_model_usage("gpt-5", "ws-alpha", 3.0),
            workspace_model_usage("gpt-5", "ws-beta", 1.0),
        ];

        let body = render_body(&mut app, 140, 8);

        assert!(
            body.contains("Workspace"),
            "expected Workspace header\n{body}"
        );
        assert!(
            body.contains("ws-alpha"),
            "expected workspace label\n{body}"
        );
        assert!(body.contains("ws-beta"), "expected workspace label\n{body}");
        assert!(body.contains("gpt-5"), "expected bare model name\n{body}");
        assert!(
            !body.contains("ws-alpha / gpt-5"),
            "model cell must not carry the workspace prefix\n{body}"
        );
    }

    #[test]
    fn models_table_omits_workspace_column_outside_workspace_grouping() {
        let mut app = make_models_app(140, GroupBy::Model);
        app.data.models = vec![workspace_model_usage("gpt-5", "ws-alpha", 3.0)];

        let body = render_body(&mut app, 140, 8);

        assert!(
            !body.contains("Workspace"),
            "Workspace column must not render under GroupBy::Model\n{body}"
        );
        assert!(body.contains("gpt-5"), "expected bare model name\n{body}");
    }

    #[test]
    fn model_detail_renders_client_and_provider_rows() {
        let mut app = make_model_detail_app(GroupBy::Model);

        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        let body = render_body(&mut app, 180, 8);

        assert!(
            body.contains("Model Details · shared-model"),
            "expected detail title\n{body}"
        );
        assert!(body.contains("Client"), "expected Client header\n{body}");
        assert!(
            body.contains("Provider"),
            "expected Provider header\n{body}"
        );
        assert!(body.contains("Claude"), "expected client rows\n{body}");
        assert!(body.contains("Anthropic"), "expected provider row\n{body}");
        assert!(body.contains("OpenRouter"), "expected provider row\n{body}");
        assert_eq!(
            body.matches("shared-model").count(),
            1,
            "locked model should render only in the title\n{body}"
        );
    }

    #[test]
    fn client_model_detail_renders_only_provider_as_identity_column() {
        let mut app = make_model_detail_app(GroupBy::ClientModel);

        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        let body = render_body(&mut app, 180, 8);

        assert!(
            body.contains("Model Details · Claude · shared-model"),
            "expected locked client and model in the title\n{body}"
        );
        assert!(
            body.contains("Provider"),
            "expected Provider header\n{body}"
        );
        assert!(body.contains("Anthropic"), "expected provider row\n{body}");
        assert!(body.contains("OpenRouter"), "expected provider row\n{body}");
        assert!(
            !body.contains("Client"),
            "locked Client column must be omitted\n{body}"
        );
        assert_eq!(
            body.matches("Claude").count(),
            1,
            "locked client should render only in the title\n{body}"
        );
        assert_eq!(
            body.matches("shared-model").count(),
            1,
            "locked model should render only in the title\n{body}"
        );
    }
}
