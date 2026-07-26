use ratatui::prelude::Constraint;

use super::table_layout::{responsive_table_layout, width_for_column, ResponsiveColumn};
use super::widgets::MODEL_DISPLAY_MAX_WIDTH;

pub(crate) const MODEL_MIN_WIDTH: u16 = 5;
pub(crate) const MODEL_MAX_WIDTH: u16 = MODEL_DISPLAY_MAX_WIDTH as u16;

// Matches the Sessions tab workspace column bounds.
pub(crate) const WORKSPACE_MIN_WIDTH: u16 = 12;
pub(crate) const WORKSPACE_MAX_WIDTH: u16 = 20;

pub(crate) const CLIENT_MIN_WIDTH: u16 = 8;
pub(crate) const CLIENT_MAX_WIDTH: u16 = 40;
pub(crate) const PROVIDER_MIN_WIDTH: u16 = 8;
pub(crate) const PROVIDER_MAX_WIDTH: u16 = 40;
pub(crate) const DETAIL_CLIENT_MAX_WIDTH: u16 = CLIENT_MAX_WIDTH;
pub(crate) const DETAIL_PROVIDER_MAX_WIDTH: u16 = PROVIDER_MAX_WIDTH;

pub(crate) const DETAIL_PROVIDER_WIDTH: u16 = PROVIDER_MIN_WIDTH;
pub(crate) const DETAIL_CLIENT_WIDTH: u16 = CLIENT_MIN_WIDTH;
pub(crate) const DETAIL_MESSAGES_WIDTH: u16 = 6;
pub(crate) const DETAIL_NUMERIC_WIDTH: u16 = 8;
pub(crate) const DETAIL_TOTAL_WIDTH: u16 = 9;
pub(crate) const DETAIL_COST_WIDTH: u16 = 9;
pub(crate) const DETAIL_COST_PER_MILLION_WIDTH: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelUsageTableDensity {
    VeryCompact,
    Core,
    Detail,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelUsageColumn {
    Workspace,
    Model,
    Client,
    Provider,
    Messages,
    Input,
    Output,
    CacheRate,
    CacheRead,
    CacheWrite,
    Total,
    Cost,
    CostPerMillion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelUsageLayoutSchema {
    Models,
    WorkspaceModels,
    ModelProviderDetails,
    ClientModelProviderDetails,
    Detail,
    WorkspaceDetail,
}

impl ModelUsageLayoutSchema {
    fn has_workspace(self) -> bool {
        matches!(self, Self::WorkspaceModels | Self::WorkspaceDetail)
    }

    fn uses_detail_metrics(self) -> bool {
        matches!(self, Self::Detail | Self::WorkspaceDetail)
    }

    fn is_model_provider_detail(self) -> bool {
        matches!(
            self,
            Self::ModelProviderDetails | Self::ClientModelProviderDetails
        )
    }

    fn locks_model(self) -> bool {
        self.is_model_provider_detail()
    }

    fn locks_client(self) -> bool {
        matches!(self, Self::ClientModelProviderDetails)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelUsageTableLayout {
    pub(crate) columns: Vec<ModelUsageColumn>,
    pub(crate) widths: Vec<Constraint>,
    pub(crate) model_width: usize,
    pub(crate) density: ModelUsageTableDensity,
}

impl ModelUsageTableLayout {
    pub(crate) fn width_for(&self, column: ModelUsageColumn) -> usize {
        width_for_column(&self.columns, &self.widths, column)
    }
}

fn column_order(column: ModelUsageColumn) -> u16 {
    match column {
        ModelUsageColumn::Workspace => 0,
        ModelUsageColumn::Model => 10,
        ModelUsageColumn::Client => 20,
        ModelUsageColumn::Provider => 30,
        ModelUsageColumn::Messages => 40,
        ModelUsageColumn::Input => 50,
        ModelUsageColumn::Output => 60,
        ModelUsageColumn::CacheRate => 70,
        ModelUsageColumn::CacheRead => 80,
        ModelUsageColumn::CacheWrite => 90,
        ModelUsageColumn::Total => 100,
        ModelUsageColumn::Cost => 110,
        ModelUsageColumn::CostPerMillion => 120,
    }
}

fn model_usage_columns(
    schema: ModelUsageLayoutSchema,
    model_content_width: u16,
    provider_content_width: u16,
    client_content_width: u16,
    workspace_content_width: u16,
) -> Vec<ResponsiveColumn<ModelUsageColumn>> {
    let client_max_width = match schema {
        ModelUsageLayoutSchema::Detail
        | ModelUsageLayoutSchema::WorkspaceDetail
        | ModelUsageLayoutSchema::ModelProviderDetails
        | ModelUsageLayoutSchema::ClientModelProviderDetails => DETAIL_CLIENT_MAX_WIDTH,
        ModelUsageLayoutSchema::Models | ModelUsageLayoutSchema::WorkspaceModels => {
            CLIENT_MAX_WIDTH
        }
    };
    let provider_max_width = match schema {
        ModelUsageLayoutSchema::Detail
        | ModelUsageLayoutSchema::WorkspaceDetail
        | ModelUsageLayoutSchema::ModelProviderDetails
        | ModelUsageLayoutSchema::ClientModelProviderDetails => DETAIL_PROVIDER_MAX_WIDTH,
        ModelUsageLayoutSchema::Models | ModelUsageLayoutSchema::WorkspaceModels => {
            PROVIDER_MAX_WIDTH
        }
    };

    let mut columns = Vec::new();
    if schema.has_workspace() {
        columns.push(ResponsiveColumn::measured_required(
            ModelUsageColumn::Workspace,
            column_order(ModelUsageColumn::Workspace),
            WORKSPACE_MIN_WIDTH,
            workspace_content_width,
            WORKSPACE_MAX_WIDTH,
        ));
    }
    if !schema.locks_model() {
        columns.push(ResponsiveColumn::measured_required(
            ModelUsageColumn::Model,
            column_order(ModelUsageColumn::Model),
            MODEL_MIN_WIDTH,
            model_content_width,
            MODEL_MAX_WIDTH,
        ));
    }
    if schema.is_model_provider_detail() {
        if !schema.locks_client() {
            columns.push(ResponsiveColumn::measured_required(
                ModelUsageColumn::Client,
                column_order(ModelUsageColumn::Client),
                CLIENT_MIN_WIDTH,
                client_content_width,
                client_max_width,
            ));
        }
        columns.push(ResponsiveColumn::measured_required(
            ModelUsageColumn::Provider,
            column_order(ModelUsageColumn::Provider),
            PROVIDER_MIN_WIDTH,
            provider_content_width,
            provider_max_width,
        ));
    }
    columns.push(ResponsiveColumn::fixed_required(
        ModelUsageColumn::Total,
        column_order(ModelUsageColumn::Total),
        DETAIL_TOTAL_WIDTH,
    ));
    columns.push(ResponsiveColumn::fixed_optional(
        ModelUsageColumn::Cost,
        10,
        column_order(ModelUsageColumn::Cost),
        DETAIL_COST_WIDTH,
    ));
    if !schema.is_model_provider_detail() {
        columns.push(ResponsiveColumn::measured_atomic_optional(
            ModelUsageColumn::Client,
            20,
            column_order(ModelUsageColumn::Client),
            CLIENT_MIN_WIDTH,
            client_content_width,
            client_max_width,
        ));
        columns.push(ResponsiveColumn::measured_atomic_optional(
            ModelUsageColumn::Provider,
            30,
            column_order(ModelUsageColumn::Provider),
            PROVIDER_MIN_WIDTH,
            provider_content_width,
            provider_max_width,
        ));
    }

    if schema.uses_detail_metrics() {
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::Messages,
            40,
            column_order(ModelUsageColumn::Messages),
            DETAIL_MESSAGES_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::Input,
            50,
            column_order(ModelUsageColumn::Input),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::Output,
            60,
            column_order(ModelUsageColumn::Output),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::CacheRate,
            70,
            column_order(ModelUsageColumn::CacheRate),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::CacheRead,
            80,
            column_order(ModelUsageColumn::CacheRead),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::CacheWrite,
            90,
            column_order(ModelUsageColumn::CacheWrite),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::CostPerMillion,
            100,
            column_order(ModelUsageColumn::CostPerMillion),
            DETAIL_COST_PER_MILLION_WIDTH,
        ));
    } else {
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::Input,
            40,
            column_order(ModelUsageColumn::Input),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::Output,
            50,
            column_order(ModelUsageColumn::Output),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::CacheRate,
            60,
            column_order(ModelUsageColumn::CacheRate),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::CacheRead,
            70,
            column_order(ModelUsageColumn::CacheRead),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::CacheWrite,
            80,
            column_order(ModelUsageColumn::CacheWrite),
            DETAIL_NUMERIC_WIDTH,
        ));
        columns.push(ResponsiveColumn::fixed_optional(
            ModelUsageColumn::CostPerMillion,
            100,
            column_order(ModelUsageColumn::CostPerMillion),
            DETAIL_COST_PER_MILLION_WIDTH,
        ));
    }

    columns
}

fn density_for_columns(columns: &[ModelUsageColumn]) -> ModelUsageTableDensity {
    if columns.contains(&ModelUsageColumn::CacheWrite) {
        ModelUsageTableDensity::Full
    } else if columns.iter().any(|column| {
        matches!(
            column,
            ModelUsageColumn::Client
                | ModelUsageColumn::Provider
                | ModelUsageColumn::Messages
                | ModelUsageColumn::Input
                | ModelUsageColumn::Output
                | ModelUsageColumn::CacheRate
                | ModelUsageColumn::CacheRead
        )
    }) {
        ModelUsageTableDensity::Detail
    } else if columns.contains(&ModelUsageColumn::Cost)
        || columns.contains(&ModelUsageColumn::CostPerMillion)
    {
        ModelUsageTableDensity::Core
    } else {
        ModelUsageTableDensity::VeryCompact
    }
}

pub(crate) fn model_subscription_table_layout(
    table_width: u16,
    model_content_width: u16,
    provider_content_width: u16,
    client_content_width: u16,
    workspace_content_width: u16,
    schema: ModelUsageLayoutSchema,
) -> ModelUsageTableLayout {
    let specs = model_usage_columns(
        schema,
        model_content_width,
        provider_content_width,
        client_content_width,
        workspace_content_width,
    );
    let layout = responsive_table_layout(table_width, &specs);
    let model_width = if layout.columns.contains(&ModelUsageColumn::Model) {
        layout.width_for(ModelUsageColumn::Model)
    } else {
        0
    };

    ModelUsageTableLayout {
        density: density_for_columns(&layout.columns),
        columns: layout.columns,
        widths: layout.widths,
        model_width,
    }
}

#[cfg(test)]
mod tests {
    use super::super::table_layout::spaced_width;
    use super::*;

    fn layout(
        table_width: u16,
        model_content_width: u16,
        provider_content_width: u16,
        client_content_width: u16,
        schema: ModelUsageLayoutSchema,
    ) -> ModelUsageTableLayout {
        model_subscription_table_layout(
            table_width,
            model_content_width,
            provider_content_width,
            client_content_width,
            0,
            schema,
        )
    }

    fn width_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    fn table_width(layout: &ModelUsageTableLayout) -> u16 {
        let widths: Vec<u16> = (0..layout.widths.len())
            .map(|index| width_at(&layout.widths, index))
            .collect();
        spaced_width(&widths)
    }

    #[test]
    fn detail_layout_keeps_model_and_tokens_at_narrow_width() {
        let layout = layout(30, 80, 40, 40, ModelUsageLayoutSchema::Detail);

        assert!(layout.columns.contains(&ModelUsageColumn::Model));
        assert!(layout.columns.contains(&ModelUsageColumn::Total));
        assert!(!layout.columns.contains(&ModelUsageColumn::Cost));
    }

    #[test]
    fn detail_layout_stops_at_wide_context_column_under_strict_priority() {
        let layout = layout(56, 80, 80, 80, ModelUsageLayoutSchema::Detail);

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Model,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&ModelUsageColumn::Client));
        assert!(!layout.columns.contains(&ModelUsageColumn::Messages));
    }

    #[test]
    fn models_schema_uses_model_and_tokens_as_core_columns() {
        let layout = layout(39, 80, 80, 80, ModelUsageLayoutSchema::Models);

        assert_eq!(
            layout.columns,
            vec![ModelUsageColumn::Model, ModelUsageColumn::Total]
        );
        assert_eq!(layout.model_width, MODEL_MAX_WIDTH as usize);
        assert_eq!(table_width(&layout), 39);
    }

    #[test]
    fn model_provider_detail_locks_model_and_keeps_varying_dimensions_required() {
        let layout = layout(27, 80, 80, 80, ModelUsageLayoutSchema::ModelProviderDetails);

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Client,
                ModelUsageColumn::Provider,
                ModelUsageColumn::Total,
            ]
        );
        assert_eq!(layout.model_width, 0);
        assert_eq!(table_width(&layout), 27);
    }

    #[test]
    fn client_model_provider_detail_locks_model_and_client() {
        let layout = layout(
            18,
            80,
            80,
            80,
            ModelUsageLayoutSchema::ClientModelProviderDetails,
        );

        assert_eq!(
            layout.columns,
            vec![ModelUsageColumn::Provider, ModelUsageColumn::Total]
        );
        assert_eq!(layout.model_width, 0);
        assert_eq!(table_width(&layout), 18);
    }

    #[test]
    fn locked_detail_dimensions_still_use_outer_metric_priority() {
        let layout = layout(37, 80, 8, 8, ModelUsageLayoutSchema::ModelProviderDetails);

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Client,
                ModelUsageColumn::Provider,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&ModelUsageColumn::Messages));
    }

    #[test]
    fn cost_is_dropped_before_model_is_sacrificed() {
        let layout = layout(39, 29, 40, 40, ModelUsageLayoutSchema::Models);

        assert_eq!(
            layout.columns,
            vec![ModelUsageColumn::Model, ModelUsageColumn::Total]
        );
        assert_eq!(layout.model_width, 29);
        assert!(!layout.columns.contains(&ModelUsageColumn::Cost));
    }

    #[test]
    fn total_tokens_column_is_required_before_cost() {
        for table_width in 1..120 {
            let layout = layout(table_width, 29, 40, 40, ModelUsageLayoutSchema::Models);

            assert!(layout.columns.contains(&ModelUsageColumn::Model));
            assert!(layout.columns.contains(&ModelUsageColumn::Total));

            if layout.columns.contains(&ModelUsageColumn::Cost) {
                assert!(layout.columns.contains(&ModelUsageColumn::Total));
            }
        }
    }

    #[test]
    fn detail_keeps_model_and_tokens_before_cost() {
        let layout = layout(30, 29, 40, 40, ModelUsageLayoutSchema::Detail);

        assert!(layout.columns.contains(&ModelUsageColumn::Model));
        assert!(layout.columns.contains(&ModelUsageColumn::Total));
        assert!(!layout.columns.contains(&ModelUsageColumn::Cost));
    }

    #[test]
    fn client_column_uses_measured_width_when_selected() {
        let layout = layout(80, 20, 80, 36, ModelUsageLayoutSchema::Models);

        assert!(layout.columns.contains(&ModelUsageColumn::Client));
        assert_eq!(layout.width_for(ModelUsageColumn::Client), 36);
    }

    #[test]
    fn models_never_show_input_before_client_when_client_has_higher_priority() {
        let layout = layout(80, 29, 40, 40, ModelUsageLayoutSchema::Models);

        assert!(layout.columns.contains(&ModelUsageColumn::Model));
        assert!(layout.columns.contains(&ModelUsageColumn::Total));
        assert!(layout.columns.contains(&ModelUsageColumn::Cost));
        assert!(!layout.columns.contains(&ModelUsageColumn::Client));
        assert!(!layout.columns.contains(&ModelUsageColumn::Provider));
        assert!(!layout.columns.contains(&ModelUsageColumn::Input));
        assert!(!layout.columns.contains(&ModelUsageColumn::Output));
    }

    #[test]
    fn models_optional_columns_are_strict_prefix_as_width_grows() {
        let priority = [
            ModelUsageColumn::Cost,
            ModelUsageColumn::Client,
            ModelUsageColumn::Provider,
            ModelUsageColumn::Input,
            ModelUsageColumn::Output,
            ModelUsageColumn::CacheRate,
            ModelUsageColumn::CacheRead,
            ModelUsageColumn::CacheWrite,
            ModelUsageColumn::CostPerMillion,
        ];
        let mut previous_len = 0usize;

        for width in 1..220 {
            let layout = layout(width, 29, 40, 40, ModelUsageLayoutSchema::Models);
            let selected: Vec<_> = priority
                .iter()
                .copied()
                .filter(|column| layout.columns.contains(column))
                .collect();

            assert_eq!(
                selected,
                priority[..selected.len()],
                "width {width} selected non-prefix optional columns: {:?}",
                layout.columns
            );
            assert!(
                selected.len() >= previous_len,
                "width {width} regressed optional prefix length"
            );

            previous_len = selected.len();
        }
    }

    #[test]
    fn detail_layout_keeps_client_priority_when_client_fits() {
        let layout = layout(90, 80, 80, 80, ModelUsageLayoutSchema::Detail);

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Model,
                ModelUsageColumn::Client,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
    }

    #[test]
    fn long_context_content_blocks_later_detail_columns_under_strict_priority() {
        let layout = layout(56, 80, 80, 80, ModelUsageLayoutSchema::Detail);

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Model,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&ModelUsageColumn::Messages));
    }

    #[test]
    fn model_width_uses_select_width_without_waiting_for_leftover() {
        let layout = layout(60, 24, 8, 8, ModelUsageLayoutSchema::Models);

        assert_eq!(layout.model_width, 24);
        assert_eq!(width_at(&layout.widths, 0) as usize, layout.model_width);
    }

    #[test]
    fn model_column_is_not_sacrificed_for_client_and_provider() {
        let layout = layout(110, 29, 40, 40, ModelUsageLayoutSchema::Models);

        assert_eq!(layout.model_width, 29);
        assert!(layout.columns.contains(&ModelUsageColumn::Model));
        assert!(layout.columns.contains(&ModelUsageColumn::Total));
    }

    #[test]
    fn optional_columns_are_dropped_before_model_is_truncated() {
        for table_width in 60..140 {
            let layout = layout(table_width, 29, 40, 40, ModelUsageLayoutSchema::Models);

            assert_eq!(
                layout.model_width, 29,
                "width {table_width} should drop optional columns before truncating model"
            );
            assert!(layout.columns.contains(&ModelUsageColumn::Total));
        }
    }

    #[test]
    fn optional_columns_are_removed_before_model_or_tokens_are_sacrificed() {
        let layout = layout(80, 29, 40, 40, ModelUsageLayoutSchema::Models);

        assert_eq!(layout.model_width, 29);
        assert!(layout.columns.contains(&ModelUsageColumn::Total));
    }

    #[test]
    fn client_and_provider_columns_stop_at_schema_caps() {
        let layout = layout(200, 80, 80, 80, ModelUsageLayoutSchema::Models);
        let client_index = layout
            .columns
            .iter()
            .position(|column| *column == ModelUsageColumn::Client)
            .expect("client should fit");
        let provider_index = layout
            .columns
            .iter()
            .position(|column| *column == ModelUsageColumn::Provider)
            .expect("provider should fit");

        assert_eq!(width_at(&layout.widths, client_index), CLIENT_MAX_WIDTH);
        assert_eq!(width_at(&layout.widths, provider_index), PROVIDER_MAX_WIDTH);
    }

    #[test]
    fn workspace_schema_splits_workspace_and_model_columns() {
        let layout = model_subscription_table_layout(
            200,
            80,
            8,
            8,
            80,
            ModelUsageLayoutSchema::WorkspaceModels,
        );

        assert_eq!(layout.columns[0], ModelUsageColumn::Workspace);
        assert_eq!(layout.columns[1], ModelUsageColumn::Model);
        assert_eq!(
            layout.width_for(ModelUsageColumn::Workspace),
            WORKSPACE_MAX_WIDTH as usize
        );
        assert_eq!(layout.model_width, MODEL_MAX_WIDTH as usize);
    }
}
