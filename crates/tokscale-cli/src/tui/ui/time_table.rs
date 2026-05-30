use ratatui::layout::Constraint;
use unicode_width::UnicodeWidthStr;

const TABLE_COLUMN_SPACING: u16 = 1;
const TIME_WIDTH: u16 = 18;
const SOURCE_MIN_WIDTH: u16 = 14;
const SOURCE_MAX_WIDTH: u16 = 40;
const TURN_WIDTH: u16 = 6;
const MSGS_WIDTH: u16 = 6;
const NUMERIC_WIDTH: u16 = 10;
const CACHE_RATE_WIDTH: u16 = 8;
const COST_WIDTH: u16 = 10;
const COST_PER_MILLION_WIDTH: u16 = 10;

pub(crate) fn display_width(s: &str) -> u16 {
    s.width().min(usize::from(u16::MAX)) as u16
}

fn spaced_width(widths: &[u16]) -> u16 {
    let spacing = TABLE_COLUMN_SPACING.saturating_mul(widths.len().saturating_sub(1) as u16);
    widths.iter().copied().sum::<u16>().saturating_add(spacing)
}

fn source_width(table_width: u16, has_turn_data: bool, source_content_width: u16) -> u16 {
    let mut widths = vec![
        TIME_WIDTH,
        SOURCE_MIN_WIDTH,
        MSGS_WIDTH,
        NUMERIC_WIDTH,
        NUMERIC_WIDTH,
        NUMERIC_WIDTH,
        NUMERIC_WIDTH,
        CACHE_RATE_WIDTH,
        NUMERIC_WIDTH,
        COST_WIDTH,
        COST_PER_MILLION_WIDTH,
    ];
    if has_turn_data {
        widths.insert(2, TURN_WIDTH);
    }

    let min_table_width = spaced_width(&widths);
    let ideal_source_width = source_content_width.clamp(SOURCE_MIN_WIDTH, SOURCE_MAX_WIDTH);
    SOURCE_MIN_WIDTH.saturating_add(
        table_width
            .saturating_sub(min_table_width)
            .min(ideal_source_width.saturating_sub(SOURCE_MIN_WIDTH)),
    )
}

pub(crate) fn full_time_table_widths(
    table_width: u16,
    has_turn_data: bool,
    source_content_width: u16,
) -> Vec<Constraint> {
    let mut widths = vec![
        Constraint::Length(TIME_WIDTH),
        Constraint::Length(source_width(
            table_width,
            has_turn_data,
            source_content_width,
        )),
    ];
    if has_turn_data {
        widths.push(Constraint::Length(TURN_WIDTH));
    }
    widths.extend([
        Constraint::Length(MSGS_WIDTH),
        Constraint::Length(NUMERIC_WIDTH),
        Constraint::Length(NUMERIC_WIDTH),
        Constraint::Length(NUMERIC_WIDTH),
        Constraint::Length(NUMERIC_WIDTH),
        Constraint::Length(CACHE_RATE_WIDTH),
        Constraint::Length(NUMERIC_WIDTH),
        Constraint::Length(COST_WIDTH),
        Constraint::Length(COST_PER_MILLION_WIDTH),
    ]);

    widths
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

    #[test]
    fn wide_time_table_source_column_expands_to_content() {
        let widths = full_time_table_widths(200, true, 16);

        assert_eq!(length_at(&widths, 1), 16);
    }

    #[test]
    fn wide_time_table_source_column_expands_without_turn_data() {
        let widths = full_time_table_widths(200, false, 16);

        assert_eq!(length_at(&widths, 1), 16);
    }

    #[test]
    fn time_table_source_column_stops_growing_after_content_fits() {
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
