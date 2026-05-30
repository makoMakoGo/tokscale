use ratatui::prelude::Constraint;
use unicode_width::UnicodeWidthStr;

use super::widgets::MODEL_DISPLAY_MAX_WIDTH;

const TABLE_COLUMN_SPACING: u16 = 1;

pub(crate) const MODEL_MIN_WIDTH: u16 = 20;
pub(crate) const MODEL_MAX_WIDTH: u16 = MODEL_DISPLAY_MAX_WIDTH as u16;
pub(crate) const PROVIDER_MAX_WIDTH: u16 = 56;
pub(crate) const SOURCE_MAX_WIDTH: u16 = 40;

pub(crate) const DETAIL_PROVIDER_WIDTH: u16 = 8;
pub(crate) const DETAIL_SOURCE_WIDTH: u16 = 12;
pub(crate) const DETAIL_MESSAGES_WIDTH: u16 = 6;
pub(crate) const DETAIL_NUMERIC_WIDTH: u16 = 8;
pub(crate) const DETAIL_TOTAL_WIDTH: u16 = 9;
pub(crate) const DETAIL_PERFORMANCE_WIDTH: u16 = 10;
pub(crate) const DETAIL_COST_WIDTH: u16 = 9;
pub(crate) const DETAIL_COST_PER_MILLION_WIDTH: u16 = 9;

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
    CostPerMillion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelUsageTableLayout {
    pub(crate) columns: Vec<ModelUsageColumn>,
    pub(crate) widths: Vec<Constraint>,
    pub(crate) model_width: usize,
    pub(crate) density: ModelUsageTableDensity,
}

pub(crate) fn display_width(s: &str) -> u16 {
    s.width().min(usize::from(u16::MAX)) as u16
}

fn clamped_content_width(content_width: u16, min: u16, max: u16) -> u16 {
    content_width.clamp(min, max)
}

fn core_model_width(table_width: u16, content_width: u16) -> u16 {
    let desired = clamped_content_width(content_width, MODEL_MIN_WIDTH, MODEL_MAX_WIDTH);
    let fixed_core_width = DETAIL_TOTAL_WIDTH
        .saturating_add(DETAIL_COST_WIDTH)
        .saturating_add(TABLE_COLUMN_SPACING.saturating_mul(2));

    desired.min(table_width.saturating_sub(fixed_core_width))
}

fn spaced_width(widths: &[u16]) -> u16 {
    let spacing = TABLE_COLUMN_SPACING.saturating_mul(widths.len().saturating_sub(1) as u16);
    widths.iter().copied().sum::<u16>().saturating_add(spacing)
}

fn column_width(
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
        ModelUsageColumn::CostPerMillion => DETAIL_COST_PER_MILLION_WIDTH,
        ModelUsageColumn::Source => source_width,
        ModelUsageColumn::Provider => provider_width,
        ModelUsageColumn::Messages => DETAIL_MESSAGES_WIDTH,
        ModelUsageColumn::Input | ModelUsageColumn::Output => DETAIL_NUMERIC_WIDTH,
        ModelUsageColumn::CacheRate => DETAIL_NUMERIC_WIDTH,
        ModelUsageColumn::CacheRead | ModelUsageColumn::CacheWrite => DETAIL_NUMERIC_WIDTH,
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
        .map(|column| column_width(*column, model_width, provider_width, source_width))
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
                | ModelUsageColumn::CostPerMillion
        )
    }) {
        ModelUsageTableDensity::Detail
    } else if columns.len() == 3 {
        ModelUsageTableDensity::Core
    } else {
        ModelUsageTableDensity::VeryCompact
    }
}

pub(crate) fn model_usage_table_layout(
    table_width: u16,
    is_very_narrow: bool,
    model_content_width: u16,
    provider_content_width: u16,
    source_content_width: u16,
    optional_columns: &[ModelUsageColumn],
) -> ModelUsageTableLayout {
    let model_width = core_model_width(table_width, model_content_width);
    let mut provider_width = DETAIL_PROVIDER_WIDTH;
    let mut source_width = DETAIL_SOURCE_WIDTH;
    let required_columns = vec![
        ModelUsageColumn::Model,
        ModelUsageColumn::Total,
        ModelUsageColumn::Cost,
    ];
    let mut columns = required_columns;

    if is_very_narrow {
        let widths = columns
            .iter()
            .map(|column| {
                Constraint::Length(column_width(
                    *column,
                    model_width,
                    provider_width,
                    source_width,
                ))
            })
            .collect();

        return ModelUsageTableLayout {
            columns,
            widths,
            model_width: model_width as usize,
            density: ModelUsageTableDensity::VeryCompact,
        };
    }

    for column in optional_columns {
        let mut candidate = columns.clone();
        let insert_at = if *column == ModelUsageColumn::CostPerMillion {
            candidate
                .iter()
                .position(|existing| *existing == ModelUsageColumn::Cost)
                .map(|index| index + 1)
                .unwrap_or(candidate.len())
        } else if *column == ModelUsageColumn::Performance {
            candidate
                .iter()
                .position(|existing| *existing == ModelUsageColumn::Cost)
                .unwrap_or(candidate.len())
        } else {
            candidate
                .iter()
                .position(|existing| {
                    matches!(existing, ModelUsageColumn::Total | ModelUsageColumn::Cost)
                })
                .unwrap_or(candidate.len())
        };
        candidate.insert(insert_at, *column);

        if layout_width(&candidate, model_width, provider_width, source_width) <= table_width {
            columns = candidate;
        }
    }

    let mut used_width = layout_width(&columns, model_width, provider_width, source_width);
    if columns.contains(&ModelUsageColumn::Source) {
        let ideal =
            clamped_content_width(source_content_width, DETAIL_SOURCE_WIDTH, SOURCE_MAX_WIDTH);
        let grow_by = table_width
            .saturating_sub(used_width)
            .min(ideal.saturating_sub(source_width));
        source_width += grow_by;
        used_width += grow_by;
    }
    if columns.contains(&ModelUsageColumn::Provider) {
        let ideal = clamped_content_width(
            provider_content_width,
            DETAIL_PROVIDER_WIDTH,
            PROVIDER_MAX_WIDTH,
        );
        let grow_by = table_width
            .saturating_sub(used_width)
            .min(ideal.saturating_sub(provider_width));
        provider_width += grow_by;
    }

    let widths = columns
        .iter()
        .map(|column| {
            Constraint::Length(column_width(
                *column,
                model_width,
                provider_width,
                source_width,
            ))
        })
        .collect();

    ModelUsageTableLayout {
        density: density_for_columns(&columns),
        columns,
        widths,
        model_width: model_width as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let layout = model_usage_table_layout(35, true, 80, 56, 40, &[]);

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Model,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
        assert_eq!(layout.model_width, 15);
        assert_eq!(table_width(&layout), 35);
    }

    #[test]
    fn core_layout_drops_optional_columns_before_overflowing_required_columns() {
        let layout = model_usage_table_layout(
            35,
            false,
            80,
            56,
            40,
            &[ModelUsageColumn::Source, ModelUsageColumn::Provider],
        );

        assert_eq!(
            layout.columns,
            vec![
                ModelUsageColumn::Model,
                ModelUsageColumn::Total,
                ModelUsageColumn::Cost,
            ]
        );
        assert_eq!(layout.model_width, 15);
        assert_eq!(table_width(&layout), 35);
    }
}
