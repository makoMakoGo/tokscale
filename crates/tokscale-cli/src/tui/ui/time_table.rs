use ratatui::layout::Constraint;

use super::table_layout::{allocate_widths, ColumnWidthSpec};

const TIME_WIDTH: u16 = 18;
const SOURCE_MIN_WIDTH: u16 = 14;
const SOURCE_MAX_WIDTH: u16 = 40;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const NUMERIC_WIDTH: u16 = 10;
const CACHE_RATE_WIDTH: u16 = 8;
const COST_WIDTH: u16 = 10;

fn full_time_table_specs(has_turn_data: bool, source_content_width: u16) -> Vec<ColumnWidthSpec> {
    let source_width = source_content_width.clamp(SOURCE_MIN_WIDTH, SOURCE_MAX_WIDTH);
    let mut specs = vec![
        ColumnWidthSpec::fixed(TIME_WIDTH),
        ColumnWidthSpec::fixed(source_width),
    ];
    if has_turn_data {
        specs.push(ColumnWidthSpec::fixed(TURN_WIDTH));
    }
    specs.extend([
        ColumnWidthSpec::fixed(MSGS_WIDTH),
        ColumnWidthSpec::fixed(NUMERIC_WIDTH),
        ColumnWidthSpec::fixed(NUMERIC_WIDTH),
        ColumnWidthSpec::fixed(NUMERIC_WIDTH),
        ColumnWidthSpec::fixed(NUMERIC_WIDTH),
        ColumnWidthSpec::fixed(CACHE_RATE_WIDTH),
        ColumnWidthSpec::fixed(NUMERIC_WIDTH),
        ColumnWidthSpec::fixed(COST_WIDTH),
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
    fn wide_time_table_source_column_uses_content_width() {
        let widths = full_time_table_widths(200, true, 16);

        assert_eq!(length_at(&widths, 1), 16);
    }

    #[test]
    fn wide_time_table_source_column_uses_content_width_without_turn_data() {
        let widths = full_time_table_widths(200, false, 16);

        assert_eq!(length_at(&widths, 1), 16);
    }

    #[test]
    fn time_table_source_column_does_not_grow_with_extra_space() {
        let fit = full_time_table_widths(200, true, 26);
        let wider = full_time_table_widths(240, true, 26);

        assert_eq!(length_at(&fit, 1), 26);
        assert_eq!(length_at(&wider, 1), 26);
    }

    #[test]
    fn time_table_source_column_stays_capped_on_very_wide_tables() {
        let widths = full_time_table_widths(260, true, 120);

        assert_eq!(length_at(&widths, 1), SOURCE_MAX_WIDTH);
    }

    #[test]
    fn time_table_source_column_keeps_minimum_for_short_content() {
        let widths = full_time_table_widths(200, true, 4);

        assert_eq!(length_at(&widths, 1), SOURCE_MIN_WIDTH);
    }

    #[test]
    fn display_width_uses_terminal_columns_for_unicode() {
        assert_eq!(display_width("模型"), 4);
        assert_eq!(display_width("e\u{301}"), 1);
    }
}
