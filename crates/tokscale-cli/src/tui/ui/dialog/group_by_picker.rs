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
                description: "One row per model; merge harnesses and providers",
            },
            GroupByOption {
                value: GroupBy::ClientModel,
                label: "Harness + Model",
                description: "One row per harness-model pair (default)",
            },
            GroupByOption {
                value: GroupBy::ClientProviderModel,
                label: "Harness + Provider + Model",
                description: "Keep provider identity; no model merging",
            },
            GroupByOption {
                value: GroupBy::WorkspaceModel,
                label: "Workspace + Model",
                description: "Group local usage by workspace, then model",
            },
        ];
        let cursor = options
            .iter()
            .position(|option| option.value == current)
            .unwrap_or(1);
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
        let value = self.options[self.cursor].value.clone();
        if *self.selected.borrow() == value {
            return InteractionOutcome::Handled;
        }
        *self.selected.borrow_mut() = value;
        *self.needs_reload.borrow_mut() = true;
        InteractionOutcome::NeedsReload
    }

    fn option_index_at(&self, area: Rect, column: u16, row: u16) -> Option<usize> {
        let list = group_by_picker_areas(area).list;
        option_index_for_row(list, self.options.len(), column, row)
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
    let bottom = list_area.bottom();
    for index in 0..option_count {
        let y = list_area.y.saturating_add((index * 2) as u16);
        if y >= bottom {
            break;
        }
        hitmap.push_row(
            Rect::new(list_area.x, y, list_area.width, 2.min(bottom - y)),
            index,
        );
    }
    hitmap.hit(column, row)
}

impl DialogContent for GroupByPickerDialog {
    fn desired_size(&self, viewport: Rect) -> (u16, u16) {
        (
            54u16.min(viewport.width.saturating_sub(4)),
            14u16.min(viewport.height.saturating_sub(4)),
        )
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        frame.render_widget(
            Block::default()
                .title(" Group By ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent)),
            area,
        );
        let areas = group_by_picker_areas(area);
        let current = self.selected.borrow();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Current: ", Style::default().fg(theme.muted)),
                Span::styled(current.to_string(), Style::default().fg(theme.accent)),
            ])),
            areas.header,
        );
        frame.render_widget(
            Paragraph::new("-".repeat(areas.divider.width as usize))
                .style(Style::default().fg(theme.border)),
            areas.divider,
        );

        let usable = areas.list.width.saturating_sub(4) as usize;
        let mut items = Vec::with_capacity(self.options.len() * 2);
        for (index, option) in self.options.iter().enumerate() {
            let is_cursor = index == self.cursor;
            let is_active = *current == option.value;
            let radio = if is_active { "(●)" } else { "( )" };
            let left = if is_active {
                format!("{radio} {}  current", option.label)
            } else {
                format!("{radio} {}", option.label)
            };
            let description = format!("    {}", option.description);
            let row_style = if is_cursor {
                Style::default()
                    .bg(theme.accent)
                    .fg(theme.background)
                    .add_modifier(Modifier::BOLD)
            } else if is_active {
                Style::default().fg(theme.foreground)
            } else {
                Style::default().fg(theme.muted)
            };
            let description_style = if is_cursor {
                Style::default().bg(theme.accent).fg(theme.background)
            } else {
                Style::default().fg(theme.muted)
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("  {left}"), row_style),
                Span::styled(
                    " ".repeat(usable.saturating_sub(left.chars().count())),
                    row_style,
                ),
            ])));
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("  {description}"), description_style),
                Span::styled(
                    " ".repeat(usable.saturating_sub(description.chars().count())),
                    description_style,
                ),
            ])));
        }
        frame.render_widget(List::new(items), areas.list);
        frame.render_widget(
            Paragraph::new("↑↓ navigate • Enter select • Esc close")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme.muted)),
            areas.hint,
        );
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

    fn make_dialog(initial: GroupBy) -> GroupByPickerDialog {
        GroupByPickerDialog::new(
            Rc::new(RefCell::new(initial)),
            Rc::new(RefCell::new(false)),
        )
    }

    #[test]
    fn picker_exposes_only_report_dimensions() {
        let dialog = make_dialog(GroupBy::ClientModel);
        assert_eq!(dialog.options.len(), 4);
        assert!(dialog
            .options
            .iter()
            .all(|option| !matches!(option.value, GroupBy::Session | GroupBy::ClientSession)));
    }

    #[test]
    fn legacy_session_selection_falls_back_to_default_cursor() {
        let dialog = make_dialog(GroupBy::Session);
        assert_eq!(dialog.cursor, 1);
    }
}
