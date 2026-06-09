use ratatui::layout::Constraint;

use super::table_layout::{allocate_widths, ColumnWidthSpec};

const TIME_WIDTH: u16 = 18;
const TIME_MAX_WIDTH: u16 = 22;
const SOURCE_MIN_WIDTH: u16 = 14;
const SOURCE_MAX_WIDTH: u16 = 40;
const SOURCE_EXTRA_WIDTH: u16 = 16;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const NUMERIC_WIDTH: u16 = 10;
const CACHE_RATE_WIDTH: u16 = 8;
const COST_WIDTH: u16 = 10;
const SMALL_COLUMN_EXTRA_WIDTH: u16 = 2;
const METRIC_EXTRA_WIDTH: u16 = 2;

fn source_max_width(source_content_width: u16) -> u16 {
    source_content_width
        .clamp(SOURCE_MIN_WIDTH, SOURCE_MAX_WIDTH)
        .saturating_add(SOURCE_EXTRA_WIDTH)
        .min(SOURCE_MAX_WIDTH)
}

fn full_time_table_specs(has_turn_data: bool, source_content_width: u16) -> Vec<ColumnWidthSpec> {
    let mut specs = vec![
        ColumnWidthSpec::flexible(TIME_WIDTH, TIME_MAX_WIDTH, 1),
        ColumnWidthSpec::flexible(SOURCE_MIN_WIDTH, source_max_width(source_content_width), 4),
    ];
    if has_turn_data {
        specs.push(ColumnWidthSpec::flexible(
            TURN_WIDTH,
            TURN_WIDTH.saturating_add(SMALL_COLUMN_EXTRA_WIDTH),
            1,
        ));
    }
    specs.extend([
        ColumnWidthSpec::flexible(
            MSGS_WIDTH,
            MSGS_WIDTH.saturating_add(SMALL_COLUMN_EXTRA_WIDTH),
            1,
        ),
        ColumnWidthSpec::flexible(
            NUMERIC_WIDTH,
            NUMERIC_WIDTH.saturating_add(METRIC_EXTRA_WIDTH),
            1,
        ),
        ColumnWidthSpec::flexible(
            NUMERIC_WIDTH,
            NUMERIC_WIDTH.saturating_add(METRIC_EXTRA_WIDTH),
            1,
        ),
        ColumnWidthSpec::flexible(
            NUMERIC_WIDTH,
            NUMERIC_WIDTH.saturating_add(METRIC_EXTRA_WIDTH),
            1,
        ),
        ColumnWidthSpec::flexible(
            NUMERIC_WIDTH,
            NUMERIC_WIDTH.saturating_add(METRIC_EXTRA_WIDTH),
            1,
        ),
        ColumnWidthSpec::flexible(
            CACHE_RATE_WIDTH,
            CACHE_RATE_WIDTH.saturating_add(METRIC_EXTRA_WIDTH),
            1,
        ),
        ColumnWidthSpec::flexible(
            NUMERIC_WIDTH,
            NUMERIC_WIDTH.saturating_add(METRIC_EXTRA_WIDTH),
            1,
        ),
        ColumnWidthSpec::flexible(COST_WIDTH, COST_WIDTH.saturating_add(METRIC_EXTRA_WIDTH), 1),
    ]);

    specs
}

pub(crate) fn full_time_table_widths(
    table_width: u16,
    has_turn_data: bool,
    source_content_width: u16,
) -> Vec<Constraint> {
    allocate_widths(
        table_width,
        &full_time_table_specs(has_turn_data, source_content_width),
    )
}

#[cfg(test)]
mod tests {
    use super::super::table_layout::display_width;
    use super::*;

    fn length_at(widths: &[Constraint], index: usize) -> u16 {
        match widths[index] {
            Constraint::Length(width) => width,
            other => panic!("expected Length at index {index}, got {other:?}"),
        }
    }

    #[test]
    fn wide_time_table_source_column_expands_to_cap() {
        let widths = full_time_table_widths(200, true, 16);

        assert_eq!(length_at(&widths, 1), 32);
    }

    #[test]
    fn wide_time_table_source_column_expands_to_cap_without_turn_data() {
        let widths = full_time_table_widths(200, false, 16);

        assert_eq!(length_at(&widths, 1), 32);
    }

    #[test]
    fn time_table_source_column_stops_after_balanced_cap() {
        let fit = full_time_table_widths(200, true, 26);
        let wider = full_time_table_widths(240, true, 26);

        assert_eq!(length_at(&fit, 1), SOURCE_MAX_WIDTH);
        assert_eq!(length_at(&wider, 1), SOURCE_MAX_WIDTH);
    }

    #[test]
    fn time_table_source_column_stays_capped_on_very_wide_tables() {
        let widths = full_time_table_widths(260, true, 120);

        assert_eq!(length_at(&widths, 1), SOURCE_MAX_WIDTH);
    }

    #[test]
    fn time_table_source_column_keeps_minimum_when_space_is_tight() {
        let widths = full_time_table_widths(80, true, 26);

        assert_eq!(length_at(&widths, 1), SOURCE_MIN_WIDTH);
    }

    #[test]
    fn display_width_uses_terminal_columns_for_unicode() {
        assert_eq!(display_width("模型"), 4);
        assert_eq!(display_width("e\u{301}"), 1);
    }
}
