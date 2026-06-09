use ratatui::prelude::Constraint;

use super::table_layout::{
    allocate_widths, choose_priority_columns, spaced_width, ColumnWidthSpec,
};
use super::widgets::MODEL_DISPLAY_MAX_WIDTH;

const TEXT_EXTRA_WIDTH: u16 = 16;
const SECONDARY_TEXT_EXTRA_WIDTH: u16 = 12;

pub(crate) const MODEL_MIN_WIDTH: u16 = 5;
pub(crate) const MODEL_MAX_WIDTH: u16 = MODEL_DISPLAY_MAX_WIDTH as u16;
pub(crate) const WORKSPACE_MODEL_MAX_WIDTH: u16 = 56;

pub(crate) const DETAIL_PROVIDER_WIDTH: u16 = 8;
pub(crate) const DETAIL_SOURCE_WIDTH: u16 = 12;
pub(crate) const DETAIL_MESSAGES_WIDTH: u16 = 6;
pub(crate) const DETAIL_NUMERIC_WIDTH: u16 = 8;
pub(crate) const DETAIL_TOTAL_WIDTH: u16 = 9;
pub(crate) const DETAIL_PERFORMANCE_WIDTH: u16 = 10;
pub(crate) const DETAIL_COST_WIDTH: u16 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelUsageTableDensity {
    VeryCompact,
    Core,
    Detail,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelUsageColumn {
    Model,
    Source,
    Provider,
    Messages,
    Input,
    Output,
    CacheRate,
    CacheRead,
    CacheWrite,
    Total,
    Performance,
    Cost,
}

pub(crate) const MODEL_USAGE_REQUIRED_COLUMNS: [ModelUsageColumn; 2] =
    [ModelUsageColumn::Model, ModelUsageColumn::Total];

pub(crate) const MODEL_USAGE_DISPLAY_ORDER: [ModelUsageColumn; 12] = [
    ModelUsageColumn::Model,
    ModelUsageColumn::Source,
    ModelUsageColumn::Provider,
    ModelUsageColumn::Messages,
    ModelUsageColumn::Input,
    ModelUsageColumn::Output,
    ModelUsageColumn::CacheRate,
    ModelUsageColumn::CacheRead,
    ModelUsageColumn::CacheWrite,
    ModelUsageColumn::Total,
    ModelUsageColumn::Cost,
    ModelUsageColumn::Performance,
];

#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelUsageLayoutProfile<'a> {
    pub(crate) required_columns: &'a [ModelUsageColumn],
    pub(crate) optional_columns_by_priority: &'a [ModelUsageColumn],
    pub(crate) display_order: &'a [ModelUsageColumn],
    pub(crate) model_max_width: u16,
}

impl<'a> ModelUsageLayoutProfile<'a> {
    pub(crate) fn standard(optional_columns_by_priority: &'a [ModelUsageColumn]) -> Self {
        Self {
            required_columns: &MODEL_USAGE_REQUIRED_COLUMNS,
            optional_columns_by_priority,
            display_order: &MODEL_USAGE_DISPLAY_ORDER,
            model_max_width: MODEL_MAX_WIDTH,
        }
    }

    pub(crate) fn workspace(optional_columns_by_priority: &'a [ModelUsageColumn]) -> Self {
        Self {
            required_columns: &MODEL_USAGE_REQUIRED_COLUMNS,
            optional_columns_by_priority,
            display_order: &MODEL_USAGE_DISPLAY_ORDER,
            model_max_width: WORKSPACE_MODEL_MAX_WIDTH,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelUsageTableLayout {
    pub(crate) columns: Vec<ModelUsageColumn>,
    pub(crate) widths: Vec<Constraint>,
    pub(crate) model_width: usize,
    pub(crate) density: ModelUsageTableDensity,
}

fn clamped_content_width(content_width: u16, min: u16, max: u16) -> u16 {
    content_width.clamp(min, max)
}

fn core_model_width(
    table_width: u16,
    content_width: u16,
    provider_width: u16,
    source_width: u16,
    profile: ModelUsageLayoutProfile<'_>,
) -> u16 {
    let desired = clamped_content_width(content_width, MODEL_MIN_WIDTH, profile.model_max_width);
    let fixed_core_width = profile
        .required_columns
        .iter()
        .filter(|column| **column != ModelUsageColumn::Model)
        .map(|column| column_base_width(*column, 0, provider_width, source_width))
        .fold(0u16, u16::saturating_add)
        .saturating_add(
            super::table_layout::TABLE_COLUMN_SPACING
                .saturating_mul(profile.required_columns.len().saturating_sub(1) as u16),
        );

    desired.min(table_width.saturating_sub(fixed_core_width))
}

fn column_base_width(
    column: ModelUsageColumn,
    model_width: u16,
    provider_width: u16,
    source_width: u16,
) -> u16 {
    match column {
        ModelUsageColumn::Model => model_width,
        ModelUsageColumn::Total => DETAIL_TOTAL_WIDTH,
        ModelUsageColumn::Performance => DETAIL_PERFORMANCE_WIDTH,
        ModelUsageColumn::Cost => DETAIL_COST_WIDTH,
        ModelUsageColumn::Source => source_width,
        ModelUsageColumn::Provider => provider_width,
        ModelUsageColumn::Messages => DETAIL_MESSAGES_WIDTH,
        ModelUsageColumn::Input | ModelUsageColumn::Output => DETAIL_NUMERIC_WIDTH,
        ModelUsageColumn::CacheRate => DETAIL_NUMERIC_WIDTH,
        ModelUsageColumn::CacheRead | ModelUsageColumn::CacheWrite => DETAIL_NUMERIC_WIDTH,
    }
}

fn column_width_spec(
    column: ModelUsageColumn,
    model_width: u16,
    provider_width: u16,
    source_width: u16,
    profile: ModelUsageLayoutProfile<'_>,
) -> ColumnWidthSpec {
    match column {
        ModelUsageColumn::Model => {
            ColumnWidthSpec::flexible(model_width, profile.model_max_width, 3)
        }
        ModelUsageColumn::Source => ColumnWidthSpec::flexible(
            source_width,
            source_width.saturating_add(TEXT_EXTRA_WIDTH),
            2,
        ),
        ModelUsageColumn::Provider => ColumnWidthSpec::flexible(
            provider_width,
            provider_width.saturating_add(SECONDARY_TEXT_EXTRA_WIDTH),
            1,
        ),
        ModelUsageColumn::Total => ColumnWidthSpec::fixed(DETAIL_TOTAL_WIDTH),
        ModelUsageColumn::Performance => ColumnWidthSpec::fixed(DETAIL_PERFORMANCE_WIDTH),
        ModelUsageColumn::Cost => ColumnWidthSpec::fixed(DETAIL_COST_WIDTH),
        ModelUsageColumn::Messages => ColumnWidthSpec::fixed(DETAIL_MESSAGES_WIDTH),
        ModelUsageColumn::Input | ModelUsageColumn::Output => {
            ColumnWidthSpec::fixed(DETAIL_NUMERIC_WIDTH)
        }
        ModelUsageColumn::CacheRate => ColumnWidthSpec::fixed(DETAIL_NUMERIC_WIDTH),
        ModelUsageColumn::CacheRead | ModelUsageColumn::CacheWrite => {
            ColumnWidthSpec::fixed(DETAIL_NUMERIC_WIDTH)
        }
    }
}

fn layout_width(
    columns: &[ModelUsageColumn],
    model_width: u16,
    provider_width: u16,
    source_width: u16,
) -> u16 {
    let widths: Vec<u16> = columns
        .iter()
        .map(|column| column_base_width(*column, model_width, provider_width, source_width))
        .collect();

    spaced_width(&widths)
}

fn density_for_columns(columns: &[ModelUsageColumn]) -> ModelUsageTableDensity {
    if columns.contains(&ModelUsageColumn::CacheWrite) {
        ModelUsageTableDensity::Full
    } else if columns.iter().any(|column| {
        matches!(
            column,
            ModelUsageColumn::Source
                | ModelUsageColumn::Provider
                | ModelUsageColumn::Messages
                | ModelUsageColumn::Input
                | ModelUsageColumn::Output
                | ModelUsageColumn::CacheRate
                | ModelUsageColumn::CacheRead
                | ModelUsageColumn::Performance
        )
    }) {
        ModelUsageTableDensity::Detail
    } else if columns.contains(&ModelUsageColumn::Cost) {
        ModelUsageTableDensity::Core
    } else {
        ModelUsageTableDensity::VeryCompact
    }
}

fn display_rank(column: ModelUsageColumn, display_order: &[ModelUsageColumn]) -> usize {
    display_order
        .iter()
        .position(|candidate| *candidate == column)
        .unwrap_or(display_order.len())
}

fn insert_by_display_order(
    candidate: &[ModelUsageColumn],
    column: ModelUsageColumn,
    display_order: &[ModelUsageColumn],
) -> usize {
    let column_rank = display_rank(column, display_order);
    candidate
        .iter()
        .position(|existing| display_rank(*existing, display_order) > column_rank)
        .unwrap_or(candidate.len())
}

pub(crate) fn model_usage_table_layout(
    table_width: u16,
    is_very_narrow: bool,
    model_content_width: u16,
    provider_content_width: u16,
    source_content_width: u16,
    profile: ModelUsageLayoutProfile<'_>,
) -> ModelUsageTableLayout {
    let provider_width = provider_content_width.max(DETAIL_PROVIDER_WIDTH);
    let source_width = source_content_width.max(DETAIL_SOURCE_WIDTH);
    let model_target_width = model_content_width.min(profile.model_max_width);
    let model_width = core_model_width(
        table_width,
        model_content_width,
        provider_width,
        source_width,
        profile,
    );
    let columns = if is_very_narrow || model_width < model_target_width {
        profile.required_columns.to_vec()
    } else {
        choose_priority_columns(
            table_width,
            profile.required_columns,
            profile.optional_columns_by_priority,
            |candidate, column| insert_by_display_order(candidate, column, profile.display_order),
            |candidate| layout_width(candidate, model_width, provider_width, source_width),
        )
    };

    if is_very_narrow || model_width < model_target_width {
        let specs: Vec<ColumnWidthSpec> = columns
            .iter()
            .map(|column| {
                column_width_spec(*column, model_width, provider_width, source_width, profile)
            })
            .collect();
        let widths = allocate_widths(table_width, &specs);
        let model_width = allocated_model_width(&columns, &widths, model_width);

        return ModelUsageTableLayout {
            columns,
            widths,
            model_width: model_width as usize,
            density: ModelUsageTableDensity::VeryCompact,
        };
    }

    let specs: Vec<ColumnWidthSpec> = columns
        .iter()
        .map(|column| {
            column_width_spec(*column, model_width, provider_width, source_width, profile)
        })
        .collect();
    let widths = allocate_widths(table_width, &specs);
    let model_width = allocated_model_width(&columns, &widths, model_width);

    ModelUsageTableLayout {
        density: density_for_columns(&columns),
        columns,
        widths,
        model_width: model_width as usize,
    }
}

fn allocated_model_width(
    columns: &[ModelUsageColumn],
    widths: &[Constraint],
    fallback: u16,
) -> u16 {
    columns
        .iter()
        .position(|column| *column == ModelUsageColumn::Model)
        .and_then(|index| match widths[index] {
            Constraint::Length(width) => Some(width),
            _ => None,
        })
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL_OPTIONAL_COLUMNS: [ModelUsageColumn; 9] = [
        ModelUsageColumn::Cost,
        ModelUsageColumn::Source,
        ModelUsageColumn::Provider,
        ModelUsageColumn::Input,
        ModelUsageColumn::Output,
        ModelUsageColumn::CacheRate,
        ModelUsageColumn::CacheRead,
        ModelUsageColumn::CacheWrite,
        ModelUsageColumn::Performance,
    ];

    fn standard_layout(
        table_width: u16,
        is_very_narrow: bool,
        model_content_width: u16,
        provider_content_width: u16,
        source_content_width: u16,
        optional_columns: &[ModelUsageColumn],
    ) -> ModelUsageTableLayout {
        model_usage_table_layout(
            table_width,
            is_very_narrow,
            model_content_width,
            provider_content_width,
            source_content_width,
            ModelUsageLayoutProfile::standard(optional_columns),
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
    fn very_narrow_layout_clamps_model_column_to_core_width() {
        let layout = standard_layout(35, true, 80, 56, 40, &MODEL_OPTIONAL_COLUMNS);

        assert_eq!(
            layout.columns,
            vec![ModelUsageColumn::Model, ModelUsageColumn::Total]
        );
        assert_eq!(layout.model_width, 25);
        assert_eq!(table_width(&layout), 35);
    }

    #[test]
    fn core_layout_drops_optional_columns_before_overflowing_required_columns() {
        let layout = standard_layout(35, false, 80, 56, 40, &MODEL_OPTIONAL_COLUMNS);

        assert_eq!(
            layout.columns,
            vec![ModelUsageColumn::Model, ModelUsageColumn::Total]
        );
        assert_eq!(layout.model_width, 25);
        assert_eq!(table_width(&layout), 35);
    }

    #[test]
    fn priority_stops_before_cost_when_it_does_not_fit() {
        let layout = standard_layout(
            48,
            false,
            80,
            16,
            16,
            &[ModelUsageColumn::Source, ModelUsageColumn::Provider],
        );

        assert_eq!(
            layout.columns,
            vec![ModelUsageColumn::Model, ModelUsageColumn::Total]
        );
        assert_eq!(table_width(&layout), 39);
    }

    #[test]
    fn lower_priority_columns_do_not_skip_a_blocked_column() {
        let layout = standard_layout(
            70,
            false,
            28,
            8,
            40,
            &[
                ModelUsageColumn::Cost,
                ModelUsageColumn::Source,
                ModelUsageColumn::Provider,
                ModelUsageColumn::Input,
            ],
        );

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Model,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
        assert!(!layout.columns.contains(&ModelUsageColumn::Provider));
        assert!(!layout.columns.contains(&ModelUsageColumn::Input));
    }

    #[test]
    fn short_model_content_uses_spare_width_after_column_selection() {
        let layout = standard_layout(
            39,
            false,
            5,
            8,
            12,
            &[
                ModelUsageColumn::Cost,
                ModelUsageColumn::Source,
                ModelUsageColumn::Provider,
            ],
        );

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Model,
                ModelUsageColumn::Source,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
        assert_eq!(layout.model_width, 6);
        assert_eq!(table_width(&layout), 39);
    }

    #[test]
    fn workspace_profile_allows_a_wider_model_column() {
        let layout = model_usage_table_layout(
            90,
            false,
            80,
            8,
            40,
            ModelUsageLayoutProfile::workspace(&MODEL_OPTIONAL_COLUMNS),
        );

        assert_eq!(layout.model_width, WORKSPACE_MODEL_MAX_WIDTH as usize);
        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Model,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
    }
}
