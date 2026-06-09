use ratatui::layout::Flex;
use ratatui::prelude::{Constraint, Margin, Rect};
use unicode_width::UnicodeWidthStr;

pub(crate) const TABLE_COLUMN_SPACING: u16 = 1;
pub(crate) const TABLE_EDGE_PADDING: u16 = 1;

/// Position selected table columns after their content widths have been chosen.
///
/// Width allocation decides which columns are visible and how wide their content
/// boxes are. This flex mode handles only the residual space: keep the first and
/// last columns pinned to the table edges, then distribute leftover space as
/// even inter-column padding.
pub(crate) const DISTRIBUTED_TABLE_FLEX: Flex = Flex::SpaceBetween;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColumnWidthSpec {
    pub(crate) width: u16,
}

impl ColumnWidthSpec {
    pub(crate) const fn fixed(width: u16) -> Self {
        Self { width }
    }
}

pub(crate) fn display_width(s: &str) -> u16 {
    s.width().min(usize::from(u16::MAX)) as u16
}

pub(crate) fn spaced_width(widths: &[u16]) -> u16 {
    let spacing = TABLE_COLUMN_SPACING.saturating_mul(widths.len().saturating_sub(1) as u16);
    widths.iter().copied().sum::<u16>().saturating_add(spacing)
}

pub(crate) fn distributed_table_area(area: Rect) -> Rect {
    area.inner(Margin {
        horizontal: TABLE_EDGE_PADDING,
        vertical: 0,
    })
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

pub(crate) fn allocate_widths(_table_width: u16, specs: &[ColumnWidthSpec]) -> Vec<Constraint> {
    specs
        .iter()
        .map(|spec| Constraint::Length(spec.width))
        .collect()
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
    fn fixed_specs_return_length_constraints() {
        let specs = [
            ColumnWidthSpec::fixed(8),
            ColumnWidthSpec::fixed(10),
            ColumnWidthSpec::fixed(6),
        ];

        let widths = allocate_widths(80, &specs);

        assert_eq!(constraint_lengths(&widths), vec![8, 10, 6]);
    }

    #[test]
    fn distributed_table_area_adds_edge_padding() {
        let area = Rect::new(10, 5, 40, 8);

        assert_eq!(distributed_table_area(area), Rect::new(11, 5, 38, 8));
    }

    #[test]
    fn display_width_uses_terminal_columns_for_unicode() {
        assert_eq!(display_width("模型"), 4);
        assert_eq!(display_width("e\u{301}"), 1);
    }

    #[test]
    fn distributed_table_flex_spreads_surplus_width_between_columns() {
        let backend = TestBackend::new(22, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let widths = [
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ];

        terminal
            .draw(|frame| {
                let table =
                    Table::new([Row::new(["A", "B", "C"])], widths).flex(DISTRIBUTED_TABLE_FLEX);
                frame.render_widget(table, distributed_table_area(frame.area()));
            })
            .unwrap();

        let line = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let a_index = line.find('A').expect("first column should render");
        let b_index = line.find('B').expect("middle column should render");
        let c_index = line.find('C').expect("last column should render");

        assert!(
            a_index == usize::from(TABLE_EDGE_PADDING),
            "first column should start after edge padding: {line}"
        );
        assert!(
            c_index == line.len() - usize::from(TABLE_EDGE_PADDING) - 1,
            "last column should end before right edge padding: {line}"
        );
        assert!(
            (4..=17).contains(&b_index),
            "middle column should receive balanced spacing: {line}"
        );
    }
}
