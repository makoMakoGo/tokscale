use ratatui::layout::Flex;
use ratatui::prelude::{Constraint, Margin, Rect};
use unicode_width::UnicodeWidthStr;

pub(crate) const TABLE_COLUMN_SPACING: u16 = 1;
pub(crate) const TABLE_EDGE_PADDING: u16 = 1;
pub(crate) const DISTRIBUTED_TABLE_FLEX: Flex = Flex::SpaceBetween;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResponsiveColumn<C> {
    pub(crate) id: C,
    pub(crate) required: bool,
    pub(crate) priority: u16,
    pub(crate) order: u16,
    pub(crate) min_width: u16,
    pub(crate) select_width: u16,
}

impl<C: Copy> ResponsiveColumn<C> {
    pub(crate) fn fixed_required(id: C, order: u16, width: u16) -> Self {
        Self {
            id,
            required: true,
            priority: 0,
            order,
            min_width: width,
            select_width: width,
        }
    }

    pub(crate) fn fixed_optional(id: C, priority: u16, order: u16, width: u16) -> Self {
        Self {
            id,
            required: false,
            priority,
            order,
            min_width: width,
            select_width: width,
        }
    }

    pub(crate) fn measured_required(
        id: C,
        order: u16,
        min_width: u16,
        content_width: u16,
        max_width: u16,
    ) -> Self {
        let width = content_width.clamp(min_width, max_width);
        Self {
            id,
            required: true,
            priority: 0,
            order,
            min_width,
            select_width: width,
        }
    }

    pub(crate) fn measured_atomic_optional(
        id: C,
        priority: u16,
        order: u16,
        min_width: u16,
        content_width: u16,
        max_width: u16,
    ) -> Self {
        let width = content_width.clamp(min_width, max_width);
        Self {
            id,
            required: false,
            priority,
            order,
            min_width: width,
            select_width: width,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResponsiveTableLayout<C> {
    pub(crate) columns: Vec<C>,
    pub(crate) widths: Vec<Constraint>,
}

impl<C: Copy + PartialEq> ResponsiveTableLayout<C> {
    pub(crate) fn width_for(&self, column: C) -> usize {
        width_for_column(&self.columns, &self.widths, column)
    }
}

pub(crate) fn width_for_column<C: Copy + PartialEq>(
    columns: &[C],
    widths: &[Constraint],
    column: C,
) -> usize {
    let index = columns
        .iter()
        .position(|candidate| *candidate == column)
        .expect("layout must contain requested column");

    match widths[index] {
        Constraint::Length(width) => width as usize,
        _ => panic!("responsive table layout must produce fixed length constraints"),
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

pub(crate) fn responsive_table_layout<C: Copy>(
    table_width: u16,
    columns: &[ResponsiveColumn<C>],
) -> ResponsiveTableLayout<C> {
    let mut selected: Vec<ResponsiveColumn<C>> = columns
        .iter()
        .copied()
        .filter(|column| column.required)
        .collect();
    selected.sort_by_key(|column| column.order);

    if select_table_width(&selected) > table_width {
        let widths = shrink_required_columns_toward_min_widths(table_width, &selected);
        return ResponsiveTableLayout {
            columns: selected.iter().map(|column| column.id).collect(),
            widths: widths.into_iter().map(Constraint::Length).collect(),
        };
    }

    let mut optional: Vec<ResponsiveColumn<C>> = columns
        .iter()
        .copied()
        .filter(|column| !column.required)
        .collect();
    optional.sort_by_key(|column| (column.priority, column.order));

    for column in optional {
        let mut candidate = selected.clone();
        candidate.push(column);
        candidate.sort_by_key(|column| column.order);

        if select_table_width(&candidate) <= table_width {
            selected = candidate;
        } else {
            break;
        }
    }

    selected.sort_by_key(|column| column.order);
    let widths = select_widths(&selected);

    ResponsiveTableLayout {
        columns: selected.iter().map(|column| column.id).collect(),
        widths: widths.into_iter().map(Constraint::Length).collect(),
    }
}

fn select_table_width<C>(columns: &[ResponsiveColumn<C>]) -> u16 {
    let widths = select_widths(columns);
    spaced_width(&widths)
}

fn select_widths<C>(columns: &[ResponsiveColumn<C>]) -> Vec<u16> {
    columns.iter().map(select_width).collect()
}

fn select_width<C>(column: &ResponsiveColumn<C>) -> u16 {
    debug_assert!(column.min_width <= column.select_width);

    column.select_width
}

// Shrinks required columns toward their minimum widths. If required minimums still
// exceed the table width, the returned layout intentionally overflows rather than
// dropping required columns.
fn shrink_required_columns_toward_min_widths<C>(
    table_width: u16,
    columns: &[ResponsiveColumn<C>],
) -> Vec<u16> {
    let mut widths = select_widths(columns);

    while spaced_width(&widths) > table_width {
        let Some(index) = columns
            .iter()
            .enumerate()
            .filter(|(index, column)| widths[*index] > column.min_width)
            .max_by_key(|(index, column)| widths[*index] - column.min_width)
            .map(|(index, _)| index)
        else {
            break;
        };

        widths[index] -= 1;
    }

    widths
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
    fn optional_selection_is_strict_priority_prefix() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum C {
            A,
            B,
            Cc,
            D,
            E,
        }

        let columns = [
            ResponsiveColumn::fixed_required(C::A, 0, 5),
            ResponsiveColumn::fixed_optional(C::B, 10, 10, 5),
            ResponsiveColumn::fixed_optional(C::Cc, 20, 20, 20),
            ResponsiveColumn::fixed_optional(C::D, 30, 30, 5),
            ResponsiveColumn::fixed_optional(C::E, 40, 40, 5),
        ];

        let layout = responsive_table_layout(17, &columns);

        assert_eq!(layout.columns, vec![C::A, C::B]);
    }

    #[test]
    fn optional_columns_cannot_force_required_columns_below_select_width() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum C {
            Name,
            Optional,
            Total,
        }

        let columns = [
            ResponsiveColumn::measured_required(C::Name, 0, 5, 12, 20),
            ResponsiveColumn::fixed_optional(C::Optional, 10, 10, 3),
            ResponsiveColumn::fixed_required(C::Total, 20, 9),
        ];

        let layout = responsive_table_layout(24, &columns);

        assert_eq!(layout.columns, vec![C::Name, C::Total]);
        assert_eq!(constraint_lengths(&layout.widths), vec![12, 9]);
    }

    #[test]
    fn required_columns_shrink_only_when_select_widths_do_not_fit() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum C {
            Name,
            Total,
        }

        let columns = [
            ResponsiveColumn::measured_required(C::Name, 0, 5, 12, 20),
            ResponsiveColumn::fixed_required(C::Total, 10, 9),
        ];

        let layout = responsive_table_layout(18, &columns);

        assert_eq!(layout.columns, vec![C::Name, C::Total]);
        assert_eq!(constraint_lengths(&layout.widths), vec![8, 9]);
    }

    #[test]
    fn required_columns_are_not_dropped_when_min_width_exceeds_table_width() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum C {
            A,
            B,
            Optional,
        }

        let columns = [
            ResponsiveColumn::fixed_required(C::A, 0, 20),
            ResponsiveColumn::fixed_required(C::B, 20, 20),
            ResponsiveColumn::fixed_optional(C::Optional, 10, 10, 1),
        ];

        let layout = responsive_table_layout(10, &columns);

        assert_eq!(layout.columns, vec![C::A, C::B]);
        assert_eq!(constraint_lengths(&layout.widths), vec![20, 20]);
    }

    #[test]
    fn distributed_table_flex_spreads_surplus_width_between_columns() {
        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                let table = Table::new(
                    [Row::new(["A", "B", "C"])],
                    [
                        Constraint::Length(1),
                        Constraint::Length(1),
                        Constraint::Length(1),
                    ],
                )
                .column_spacing(TABLE_COLUMN_SPACING)
                .flex(DISTRIBUTED_TABLE_FLEX);

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
        let a = line.find('A').expect("first column should render");
        let b = line.find('B').expect("middle column should render");
        let c = line.find('C').expect("last column should render");

        assert_eq!(a, usize::from(TABLE_EDGE_PADDING));
        assert_eq!(c, line.len() - usize::from(TABLE_EDGE_PADDING) - 1);
        assert!(b > a + 2, "middle column should be spaced out: {line}");
        assert!(b < c - 2, "middle column should be spaced out: {line}");
    }
}
