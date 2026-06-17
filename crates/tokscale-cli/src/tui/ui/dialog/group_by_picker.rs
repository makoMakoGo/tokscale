use std::cell::RefCell;
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use tokscale_core::GroupBy;

use crate::tui::interaction::{HitMap, InteractionOutcome, ListInteraction, MoveCommand, WrapMode};
use crate::tui::themes::Theme;

use super::{DialogContent, DialogResult};

pub struct GroupByPickerDialog {
    options: Vec<GroupByOption>,
    selected: Rc<RefCell<GroupBy>>,
    needs_reload: Rc<RefCell<bool>>,
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
    pub fn new(selected: Rc<RefCell<GroupBy>>, needs_reload: Rc<RefCell<bool>>) -> Self {
        let current = selected.borrow().clone();
        let options = vec![
            GroupByOption {
                value: GroupBy::Model,
                label: "Model",
                description: "One row per model (merge clients & providers)",
            },
            GroupByOption {
                value: GroupBy::ClientModel,
                label: "Client + Model",
                description: "One row per client-model pair (default)",
            },
            GroupByOption {
                value: GroupBy::ClientProviderModel,
                label: "Client + Provider + Model",
                description: "Most granular — no merging",
            },
            GroupByOption {
                value: GroupBy::WorkspaceModel,
                label: "Workspace + Model",
                description: "Group local usage by workspace key, then model",
            },
            GroupByOption {
                value: GroupBy::Session,
                label: "Session + Model",
                description: "One row per session_id and model (attribute cost per session)",
            },
            GroupByOption {
                value: GroupBy::ClientSession,
                label: "Client + Session + Model",
                description: "One row per client, session_id, and model",
            },
        ];

        let cursor = options.iter().position(|o| o.value == current).unwrap_or(1);

        Self {
            options,
            selected,
            needs_reload,
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

    fn select_current(&mut self) -> InteractionOutcome {
        let new_value = self.options[self.cursor].value.clone();
        let changed = *self.selected.borrow() != new_value;
        if changed {
            *self.selected.borrow_mut() = new_value;
            *self.needs_reload.borrow_mut() = true;
            InteractionOutcome::NeedsReload
        } else {
            InteractionOutcome::Handled
        }
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
        // 6 options render as 2 lines each (label + description) = 12 rows,
        // plus header (1) + divider (1) + hint (1) + borders (2). Cap at 18
        // so every option stays visible without scrolling on a typical
        // terminal; matches source_picker's sizing.
        let width = 52u16.min(viewport.width.saturating_sub(4));
        let height = 18u16.min(viewport.height.saturating_sub(4));
        (width, height)
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(" Group By ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent));
        frame.render_widget(block, area);

        let rows = group_by_picker_areas(area);

        let current = self.selected.borrow();
        let header = Paragraph::new(Line::from(vec![
            Span::styled("Current: ", Style::default().fg(theme.muted)),
            Span::styled(current.to_string(), Style::default().fg(theme.accent)),
        ]));
        frame.render_widget(header, rows.header);

        let divider = Paragraph::new("-".repeat(rows.divider.width as usize))
            .style(Style::default().fg(theme.border));
        frame.render_widget(divider, rows.divider);

        let list_area = rows.list;
        let mut items: Vec<ListItem> = Vec::new();

        for (i, opt) in self.options.iter().enumerate() {
            let is_cursor = i == self.cursor;
            let is_active = *current == opt.value;

            let radio = if is_active { "(●)" } else { "( )" };
            let usable = list_area.width.saturating_sub(4) as usize;
            let left = if is_active {
                format!("{} {}  current", radio, opt.label)
            } else {
                format!("{} {}", radio, opt.label)
            };
            let desc = format!("    {}", opt.description);

            let base_style = if is_cursor {
                Style::default()
                    .bg(theme.accent)
                    .fg(theme.background)
                    .add_modifier(Modifier::BOLD)
            } else if is_active {
                Style::default().fg(theme.foreground)
            } else {
                Style::default().fg(theme.muted)
            };

            let desc_style = if is_cursor {
                Style::default().bg(theme.accent).fg(theme.background)
            } else {
                Style::default().fg(theme.muted)
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
            .style(Style::default().fg(theme.muted));
        frame.render_widget(hint, rows.hint);
    }

    fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        match key.code {
            KeyCode::Esc => DialogResult::Close,
            KeyCode::Up => self.move_cursor(MoveCommand::Up).into(),
            KeyCode::Down => self.move_cursor(MoveCommand::Down).into(),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let _ = self.select_current();
                DialogResult::Close
            }
            _ => DialogResult::Ignored("unhandled key"),
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent, area: Rect) -> DialogResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = self.option_index_at(area, event.column, event.row) {
                    self.cursor = index;
                    let _ = self.select_current();
                    DialogResult::Close
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
        let selected = Rc::new(RefCell::new(initial));
        let needs_reload = Rc::new(RefCell::new(false));
        GroupByPickerDialog::new(selected, needs_reload)
    }

    fn render_symbols(dialog: &GroupByPickerDialog, area: Rect) -> String {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
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

        let rendered = render_symbols(&dialog, Rect::new(0, 0, 52, 18));

        assert!(rendered.contains("(●) Client + Model  current"));
    }

    #[test]
    fn group_by_picker_mouse_hitbox_selects_label_row() {
        let mut dialog = make_dialog(GroupBy::ClientModel);
        let area = Rect::new(0, 0, 52, 18);
        let list = group_by_picker_areas(area).list;

        let result = dialog.handle_mouse(click(list.x, list.y + 6), area);

        assert!(matches!(result, DialogResult::Close));
        assert_eq!(*dialog.selected.borrow(), GroupBy::WorkspaceModel);
        assert!(*dialog.needs_reload.borrow());
    }

    #[test]
    fn group_by_picker_mouse_hitbox_selects_description_row() {
        let mut dialog = make_dialog(GroupBy::ClientModel);
        let area = Rect::new(0, 0, 52, 18);
        let list = group_by_picker_areas(area).list;

        let result = dialog.handle_mouse(click(list.x, list.y + 7), area);

        assert!(matches!(result, DialogResult::Close));
        assert_eq!(*dialog.selected.borrow(), GroupBy::WorkspaceModel);
        assert!(*dialog.needs_reload.borrow());
    }

    #[test]
    fn group_by_picker_mouse_outside_rows_does_not_select() {
        let mut dialog = make_dialog(GroupBy::ClientModel);
        let area = Rect::new(0, 0, 52, 18);
        let list = group_by_picker_areas(area).list;

        let result = dialog.handle_mouse(click(list.x, list.y + 12), area);

        assert!(matches!(
            result,
            DialogResult::Ignored("click outside rows")
        ));
        assert_eq!(*dialog.selected.borrow(), GroupBy::ClientModel);
        assert!(!*dialog.needs_reload.borrow());
    }
}
