use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use tokenx_engine::GroupBy;

use crate::tui::interaction::{HitMap, InteractionOutcome, ListInteraction, MoveCommand, WrapMode};
use crate::tui::themes::Theme;

use super::{DialogContent, DialogResult, UiCommand};

pub struct GroupByPickerDialog {
    options: Vec<GroupByOption>,
    current: GroupBy,
    cursor: usize,
}

struct GroupByOption {
    value: GroupBy,
    label: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GroupByPickerAreas {
    header: Rect,
    divider: Rect,
    list: Rect,
    hint: Rect,
}

impl GroupByPickerDialog {
    pub fn new(current: GroupBy) -> Self {
        let options = vec![
            GroupByOption {
                value: GroupBy::Model,
                label: "Model",
                description: "One row per model; merge clients and providers (default)",
            },
            GroupByOption {
                value: GroupBy::ClientModel,
                label: "Client + Model",
                description: "One row per client-model pair",
            },
            GroupByOption {
                value: GroupBy::ClientProviderModel,
                label: "Client + Provider + Model",
                description: "Keep provider identity; no model merging",
            },
            GroupByOption {
                value: GroupBy::WorkspaceModel,
                label: "Workspace + Model",
                description: "Group local usage by workspace, then model",
            },
        ];

        let cursor = options.iter().position(|o| o.value == current).unwrap_or(0);

        Self {
            options,
            current,
            cursor,
        }
    }

    fn move_cursor(&mut self, command: MoveCommand) -> InteractionOutcome {
        let mut interaction = ListInteraction {
            selected: self.cursor,
            scroll: 0,
            visible: self.options.len().max(1),
        };
        let outcome = interaction.apply_move(command, self.options.len(), WrapMode::Wrap);
        self.cursor = interaction.selected;
        outcome
    }

    fn submit_current(&self) -> DialogResult {
        DialogResult::Submit(UiCommand::ProjectGroupBy(self.options[self.cursor].value))
    }

    fn option_index_at(&self, area: Rect, column: u16, row: u16) -> Option<usize> {
        let list_area = group_by_picker_areas(area).list;
        option_index_for_row(list_area, self.options.len(), column, row)
    }
}

fn group_by_picker_areas(area: Rect) -> GroupByPickerAreas {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(inner);

    GroupByPickerAreas {
        header: rows[0],
        divider: rows[1],
        list: rows[2],
        hint: rows[3],
    }
}

fn option_index_for_row(
    list_area: Rect,
    option_count: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    let mut hitmap = HitMap::default();
    let bottom = list_area.y.saturating_add(list_area.height);

    for index in 0..option_count {
        let option_y = list_area.y.saturating_add((index * 2) as u16);
        if option_y >= bottom {
            break;
        }

        let height = 2.min(bottom.saturating_sub(option_y));
        hitmap.push_row(
            Rect::new(list_area.x, option_y, list_area.width, height),
            index,
        );
    }

    let hit = hitmap.hit(column, row);
    hitmap.clear();
    hit
}

impl DialogContent for GroupByPickerDialog {
    fn desired_size(&self, viewport: Rect) -> (u16, u16) {
        // Four options use two rows each, plus header, divider, hint, and borders.
        let width = 54u16.min(viewport.width.saturating_sub(4));
        let height = 14u16.min(viewport.height.saturating_sub(4));
        (width, height)
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(" Group By ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.chrome.focus));
        frame.render_widget(block, area);

        let rows = group_by_picker_areas(area);

        let header = Paragraph::new(Line::from(vec![
            Span::styled("Current: ", Style::default().fg(theme.text.secondary)),
            Span::styled(
                self.current.to_string(),
                Style::default().fg(theme.chrome.current),
            ),
        ]));
        frame.render_widget(header, rows.header);

        let divider = Paragraph::new("-".repeat(rows.divider.width as usize))
            .style(Style::default().fg(theme.chrome.border));
        frame.render_widget(divider, rows.divider);

        let list_area = rows.list;
        let mut items: Vec<ListItem> = Vec::new();

        for (i, opt) in self.options.iter().enumerate() {
            let is_cursor = i == self.cursor;
            let is_active = self.current == opt.value;

            let radio = if is_active { "(●)" } else { "( )" };
            let usable = list_area.width.saturating_sub(4) as usize;
            let left = if is_active {
                format!("{} {}  current", radio, opt.label)
            } else {
                format!("{} {}", radio, opt.label)
            };
            let desc = format!("    {}", opt.description);

            let base_style = if is_cursor {
                theme.selection_style()
            } else if is_active {
                Style::default().fg(theme.chrome.current)
            } else {
                Style::default().fg(theme.text.secondary)
            };

            let desc_style = if is_cursor {
                theme.selection_style()
            } else {
                Style::default().fg(theme.text.secondary)
            };

            let padding = usable.saturating_sub(left.chars().count());
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("  {}", left), base_style),
                Span::styled(" ".repeat(padding), base_style),
            ])));

            let desc_padding = usable.saturating_sub(desc.chars().count());
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("  {}", desc), desc_style),
                Span::styled(" ".repeat(desc_padding), desc_style),
            ])));
        }

        frame.render_widget(List::new(items), list_area);

        let hint = Paragraph::new("↑↓ navigate • Enter select • Esc close")
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.text.secondary));
        frame.render_widget(hint, rows.hint);
    }

    fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        match key.code {
            KeyCode::Esc => DialogResult::Close,
            KeyCode::Up => self.move_cursor(MoveCommand::Up).into(),
            KeyCode::Down => self.move_cursor(MoveCommand::Down).into(),
            KeyCode::Enter | KeyCode::Char(' ') => self.submit_current(),
            _ => DialogResult::Ignored("unhandled key"),
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent, area: Rect) -> DialogResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = self.option_index_at(area, event.column, event.row) {
                    self.cursor = index;
                    self.submit_current()
                } else {
                    DialogResult::Ignored("click outside rows")
                }
            }
            MouseEventKind::ScrollUp => self.move_cursor(MoveCommand::Up).into(),
            MouseEventKind::ScrollDown => self.move_cursor(MoveCommand::Down).into(),
            _ => DialogResult::Ignored("unhandled mouse"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{backend::TestBackend, Terminal};

    use crate::tui::themes::{Theme, ThemeName};

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn make_dialog(initial: GroupBy) -> GroupByPickerDialog {
        GroupByPickerDialog::new(initial)
    }

    fn render_symbols(dialog: &GroupByPickerDialog, area: Rect) -> String {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        let theme = Theme::from_name(ThemeName::Blue);
        let frame = terminal
            .draw(|frame| {
                dialog.render(frame, area, &theme);
            })
            .unwrap();

        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| frame.buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn group_by_picker_current_selection_is_rendered_with_current_marker() {
        let mut dialog = make_dialog(GroupBy::ClientModel);
        dialog.cursor = 0;

        let rendered = render_symbols(&dialog, Rect::new(0, 0, 54, 14));

        assert!(rendered.contains("(●) Client + Model  current"));
    }

    #[test]
    fn group_by_picker_mouse_hitbox_selects_label_row() {
        let mut dialog = make_dialog(GroupBy::ClientModel);
        let area = Rect::new(0, 0, 54, 14);
        let list = group_by_picker_areas(area).list;

        let result = dialog.handle_mouse(click(list.x, list.y + 6), area);

        assert_eq!(
            result,
            DialogResult::Submit(UiCommand::ProjectGroupBy(GroupBy::WorkspaceModel))
        );
        assert_eq!(dialog.current, GroupBy::ClientModel);
    }

    #[test]
    fn group_by_picker_mouse_hitbox_selects_description_row() {
        let mut dialog = make_dialog(GroupBy::ClientModel);
        let area = Rect::new(0, 0, 54, 14);
        let list = group_by_picker_areas(area).list;

        let result = dialog.handle_mouse(click(list.x, list.y + 7), area);

        assert_eq!(
            result,
            DialogResult::Submit(UiCommand::ProjectGroupBy(GroupBy::WorkspaceModel))
        );
        assert_eq!(dialog.current, GroupBy::ClientModel);
    }

    #[test]
    fn group_by_picker_mouse_outside_rows_does_not_select() {
        let mut dialog = make_dialog(GroupBy::ClientModel);
        let area = Rect::new(0, 0, 54, 14);
        let list = group_by_picker_areas(area).list;

        let result = dialog.handle_mouse(click(list.x, list.y + 8), area);

        assert!(matches!(
            result,
            DialogResult::Ignored("click outside rows")
        ));
        assert_eq!(dialog.current, GroupBy::ClientModel);
    }

    #[test]
    fn group_by_picker_exposes_only_report_dimensions() {
        let dialog = make_dialog(GroupBy::ClientModel);

        assert_eq!(
            dialog
                .options
                .iter()
                .map(|option| option.value)
                .collect::<Vec<_>>(),
            vec![
                GroupBy::Model,
                GroupBy::ClientModel,
                GroupBy::ClientProviderModel,
                GroupBy::WorkspaceModel,
            ]
        );
    }
}
