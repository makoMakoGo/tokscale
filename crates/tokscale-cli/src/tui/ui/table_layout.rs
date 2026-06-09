use ratatui::layout::Flex;
use ratatui::prelude::Constraint;
use unicode_width::UnicodeWidthStr;

pub(crate) const TABLE_COLUMN_SPACING: u16 = 1;
pub(crate) const PRIMARY_TABLE_FLEX: Flex = Flex::SpaceBetween;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColumnWidthSpec {
    pub(crate) base: u16,
    pub(crate) max: u16,
    pub(crate) flex: u16,
}

impl ColumnWidthSpec {
    pub(crate) const fn fixed(width: u16) -> Self {
        Self {
            base: width,
            max: width,
            flex: 0,
        }
    }

    pub(crate) fn flexible(base: u16, max: u16, flex: u16) -> Self {
        Self {
            base,
            max: max.max(base),
            flex,
        }
    }
}

pub(crate) fn display_width(s: &str) -> u16 {
    s.width().min(usize::from(u16::MAX)) as u16
}

pub(crate) fn spaced_width(widths: &[u16]) -> u16 {
    let spacing = TABLE_COLUMN_SPACING.saturating_mul(widths.len().saturating_sub(1) as u16);
    widths.iter().copied().sum::<u16>().saturating_add(spacing)
}

pub(crate) fn choose_priority_columns<T, InsertAt, Width>(
    table_width: u16,
    required_columns: &[T],
    optional_columns: &[T],
    mut insert_at: InsertAt,
    mut width: Width,
) -> Vec<T>
where
    T: Copy,
    InsertAt: FnMut(&[T], T) -> usize,
    Width: FnMut(&[T]) -> u16,
{
    let mut columns = required_columns.to_vec();

    for column in optional_columns {
        let mut candidate = columns.clone();
        let insert_at = insert_at(&candidate, *column).min(candidate.len());
        candidate.insert(insert_at, *column);

        if width(&candidate) <= table_width {
            columns = candidate;
        } else {
            break;
        }
    }

    columns
}

pub(crate) fn allocate_widths(table_width: u16, specs: &[ColumnWidthSpec]) -> Vec<Constraint> {
    let mut widths: Vec<u16> = specs.iter().map(|spec| spec.base.min(spec.max)).collect();
    let mut remaining = table_width.saturating_sub(spaced_width(&widths));

    while remaining > 0 {
        let mut progressed = false;

        for (index, spec) in specs.iter().enumerate() {
            if spec.flex == 0 || widths[index] >= spec.max {
                continue;
            }

            for _ in 0..spec.flex {
                if remaining == 0 || widths[index] >= spec.max {
                    break;
                }

                widths[index] = widths[index].saturating_add(1);
                remaining -= 1;
                progressed = true;
            }
        }

        if !progressed {
            break;
        }
    }

    widths.into_iter().map(Constraint::Length).collect()
}

#[cfg(test)]
pub(crate) fn constraint_lengths(widths: &[Constraint]) -> Vec<u16> {
    widths
        .iter()
        .map(|constraint| match constraint {
            Constraint::Length(width) => *width,
            other => panic!("expected Length constraint, got {other:?}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::{Row, Table};
    use ratatui::Terminal;

    #[test]
    fn exact_fit_returns_base_widths() {
        let specs = [
            ColumnWidthSpec::fixed(8),
            ColumnWidthSpec::flexible(10, 20, 2),
            ColumnWidthSpec::fixed(6),
        ];

        let widths = allocate_widths(26, &specs);

        assert_eq!(constraint_lengths(&widths), vec![8, 10, 6]);
    }

    #[test]
    fn surplus_width_goes_to_flexible_columns_by_weight() {
        let specs = [
            ColumnWidthSpec::flexible(10, 20, 3),
            ColumnWidthSpec::fixed(8),
            ColumnWidthSpec::flexible(10, 20, 1),
        ];

        let widths = allocate_widths(36, &specs);

        assert_eq!(constraint_lengths(&widths), vec![15, 8, 11]);
    }

    #[test]
    fn capped_columns_stop_growing() {
        let specs = [
            ColumnWidthSpec::flexible(10, 12, 3),
            ColumnWidthSpec::flexible(10, 18, 1),
        ];

        let widths = allocate_widths(60, &specs);

        assert_eq!(constraint_lengths(&widths), vec![12, 18]);
    }

    #[test]
    fn fixed_numeric_columns_do_not_expand() {
        let specs = [
            ColumnWidthSpec::flexible(10, 20, 1),
            ColumnWidthSpec::fixed(8),
            ColumnWidthSpec::fixed(8),
        ];

        let widths = allocate_widths(40, &specs);

        assert_eq!(constraint_lengths(&widths), vec![20, 8, 8]);
    }

    #[test]
    fn display_width_uses_terminal_columns_for_unicode() {
        assert_eq!(display_width("模型"), 4);
        assert_eq!(display_width("e\u{301}"), 1);
    }

    #[test]
    fn primary_table_flex_spreads_surplus_width_between_columns() {
        let backend = TestBackend::new(20, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let widths = [
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ];

        terminal
            .draw(|frame| {
                let table =
                    Table::new([Row::new(["A", "B", "C"])], widths).flex(PRIMARY_TABLE_FLEX);
                frame.render_widget(table, frame.area());
            })
            .unwrap();

        let line = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let b_index = line.find('B').expect("middle column should render");

        assert!(
            line.starts_with('A'),
            "first column should stay at start: {line}"
        );
        assert!(
            line.trim_end().ends_with('C'),
            "last column should not leave trailing surplus: {line}"
        );
        assert!(
            (4..=15).contains(&b_index),
            "middle column should receive balanced spacing: {line}"
        );
    }
}
