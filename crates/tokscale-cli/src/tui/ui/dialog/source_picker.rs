use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use tokscale_core::ClientId;

use crate::tui::client_ui;
use crate::tui::interaction::{InteractionOutcome, ListInteraction};
use crate::tui::themes::Theme;

use super::{DialogContent, DialogResult};

/// TUI dialog that lets the user toggle which clients are included in reports.
/// Backed by the same unified
/// `Rc<RefCell<HashSet<ClientId>>>` the rest of the app sees, so
/// toggles propagate without a separate sync step.
pub struct ClientPickerDialog {
    /// Every selectable filter in the same order they appear on screen.
    /// Mirrors `ClientId::ALL` so the listing order is
    /// the canonical chronological order across the whole CLI/TUI.
    sources: Vec<ClientId>,
    enabled: Rc<RefCell<HashSet<ClientId>>>,
    needs_reload: Rc<RefCell<bool>>,
    selected: usize,
    filter: String,
    /// Indices into `sources` that match the current type-to-filter
    /// substring. `selected` indexes into this vec, not into `sources`.
    filtered_indices: Vec<usize>,
    last_error: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourcePickerAreas {
    filter: Rect,
    divider: Rect,
    list: Rect,
    hint: Rect,
}

impl ClientPickerDialog {
    pub fn new(enabled: Rc<RefCell<HashSet<ClientId>>>, needs_reload: Rc<RefCell<bool>>) -> Self {
        let sources: Vec<ClientId> = ClientId::ALL.to_vec();
        let filtered_indices: Vec<usize> = (0..sources.len()).collect();
        Self {
            sources,
            enabled,
            needs_reload,
            selected: 0,
            filter: String::new(),
            filtered_indices,
            last_error: None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered_indices.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.filtered_indices.len() as isize;
        let mut next = self.selected as isize + delta;
        if next < 0 {
            next = max - 1;
        } else if next >= max {
            next = 0;
        }
        self.selected = next as usize;
    }

    /// Toggle the currently highlighted source. Refuses to disable the
    /// last enabled source (downstream code assumes at least one
    /// filter is active when the picker is in use).
    fn toggle_selected(&mut self) -> InteractionOutcome {
        if let Some(&idx) = self.filtered_indices.get(self.selected) {
            self.toggle(self.sources[idx])
        } else {
            InteractionOutcome::Ignored("empty filtered list")
        }
    }

    fn toggle(&mut self, client: ClientId) -> InteractionOutcome {
        let mut enabled = self.enabled.borrow_mut();
        let total = enabled.len();
        let is_enabled = enabled.contains(&client);

        if is_enabled && total > 1 {
            enabled.remove(&client);
            *self.needs_reload.borrow_mut() = true;
            self.last_error = None;
            InteractionOutcome::NeedsReload
        } else if !is_enabled {
            enabled.insert(client);
            *self.needs_reload.borrow_mut() = true;
            self.last_error = None;
            InteractionOutcome::NeedsReload
        } else {
            self.last_error = Some("Cannot disable the last source");
            InteractionOutcome::Ignored("last source")
        }
    }

    fn rebuild_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        if needle.is_empty() {
            self.filtered_indices = (0..self.sources.len()).collect();
        } else {
            self.filtered_indices = self
                .sources
                .iter()
                .enumerate()
                .filter(|(_, c)| display_name(**c).to_lowercase().contains(&needle))
                .map(|(i, _)| i)
                .collect();
        }
        if self.selected >= self.filtered_indices.len() {
            self.selected = 0;
        }
    }

    fn visible_scroll(&self, visible_height: usize) -> usize {
        let len = self.filtered_indices.len();
        let mut interaction = ListInteraction::default();
        interaction.set_visible(visible_height, len);
        let _ = interaction.select(self.selected, len);
        interaction.visible_range(len).start
    }

    fn filtered_row_at(&self, area: Rect, column: u16, row: u16) -> Option<(usize, ClientId)> {
        let areas = source_picker_areas(area);
        if column < areas.list.x
            || column >= areas.list.x.saturating_add(areas.list.width)
            || row < areas.list.y
            || row >= areas.list.y.saturating_add(areas.list.height)
        {
            return None;
        }

        let visible_height = areas.list.height as usize;
        let flat_idx = self
            .visible_scroll(visible_height)
            .saturating_add(row.saturating_sub(areas.list.y) as usize);
        let source_idx = *self.filtered_indices.get(flat_idx)?;
        Some((flat_idx, self.sources[source_idx]))
    }

    #[cfg(test)]
    fn client_at(&self, area: Rect, column: u16, row: u16) -> Option<ClientId> {
        self.filtered_row_at(area, column, row)
            .map(|(_, client)| client)
    }
}

fn source_picker_areas(area: Rect) -> SourcePickerAreas {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(inner);

    SourcePickerAreas {
        filter: rows[0],
        divider: rows[1],
        list: rows[2],
        hint: rows[3],
    }
}

impl DialogContent for ClientPickerDialog {
    fn desired_size(&self, viewport: Rect) -> (u16, u16) {
        let width = 50u16.min(viewport.width.saturating_sub(4));
        let height = 18u16.min(viewport.height.saturating_sub(4));
        (width, height)
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let block = Block::default()
            .title(" Clients ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent));
        frame.render_widget(block, area);

        let rows = source_picker_areas(area);

        let filter_text = if self.filter.is_empty() {
            Span::styled("Type to filter...", Style::default().fg(theme.muted))
        } else {
            Span::styled(&self.filter, Style::default().fg(theme.foreground))
        };
        let filter_line = Paragraph::new(Line::from(vec![
            Span::styled("Filter: ", Style::default().fg(theme.accent)),
            filter_text,
        ]));
        frame.render_widget(filter_line, rows.filter);

        let divider = Paragraph::new("-".repeat(rows.divider.width as usize))
            .style(Style::default().fg(theme.border));
        frame.render_widget(divider, rows.divider);

        let list_area = rows.list;
        let visible_height = list_area.height as usize;
        let scroll = self.visible_scroll(visible_height);

        let mut items: Vec<ListItem> = Vec::new();
        for (flat_idx, &idx) in self.filtered_indices.iter().enumerate() {
            if flat_idx < scroll {
                continue;
            }
            if items.len() >= visible_height {
                break;
            }

            let source = self.sources[idx];
            let is_selected = flat_idx == self.selected;
            let is_enabled = self.enabled.borrow().contains(&source);

            let checkbox = if is_enabled { "[●]" } else { "[ ]" };
            let key_hint = format!("[{}]", hotkey(source));
            let name = display_name(source);

            let usable = list_area.width.saturating_sub(4) as usize;
            let left = format!("{} {} {}", checkbox, key_hint, name);
            let padding = usable.saturating_sub(left.chars().count());

            let base_style = if is_selected {
                Style::default()
                    .bg(theme.accent)
                    .fg(theme.background)
                    .add_modifier(Modifier::BOLD)
            } else if is_enabled {
                Style::default().fg(theme.foreground)
            } else {
                Style::default().fg(theme.muted)
            };

            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("  {}", left), base_style),
                Span::styled(" ".repeat(padding), base_style),
            ])));
        }

        if items.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "  No results",
                Style::default().fg(theme.muted),
            ))));
        }

        frame.render_widget(List::new(items), list_area);

        let hint_text = self.last_error.unwrap_or(
            "↑↓ navigate • Enter toggle • type filter • Alt+hotkey toggle • Backspace edit • Esc close",
        );
        let hint_style = if self.last_error.is_some() {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(theme.muted)
        };
        let hint = Paragraph::new(hint_text)
            .alignment(Alignment::Center)
            .style(hint_style);
        frame.render_widget(hint, rows.hint);
    }

    fn handle_key(&mut self, key: KeyEvent) -> DialogResult {
        match key.code {
            KeyCode::Esc => DialogResult::Close,
            KeyCode::Up => {
                self.move_selection(-1);
                DialogResult::Handled
            }
            KeyCode::Down => {
                self.move_selection(1);
                DialogResult::Handled
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_selected().into(),
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild_filter();
                self.last_error = None;
                DialogResult::Handled
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::ALT) {
                    if let Some(client_id) = client_ui::from_hotkey(c) {
                        self.toggle(client_id).into()
                    } else {
                        DialogResult::Ignored("unknown hotkey")
                    }
                } else {
                    self.filter.push(c);
                    self.rebuild_filter();
                    self.last_error = None;
                    DialogResult::Handled
                }
            }
            _ => DialogResult::Ignored("unhandled key"),
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent, area: Rect) -> DialogResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((filtered_idx, client)) =
                    self.filtered_row_at(area, event.column, event.row)
                {
                    self.selected = filtered_idx;
                    self.toggle(client).into()
                } else {
                    DialogResult::Ignored("click outside rows")
                }
            }
            MouseEventKind::ScrollUp => {
                self.move_selection(-1);
                DialogResult::Handled
            }
            MouseEventKind::ScrollDown => {
                self.move_selection(1);
                DialogResult::Handled
            }
            _ => DialogResult::Ignored("unhandled mouse"),
        }
    }
}

fn display_name(client: ClientId) -> &'static str {
    client_ui::display_name(client)
}

fn hotkey(client: ClientId) -> char {
    client_ui::hotkey(client).expect("source picker clients must have catalog hotkeys")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn alt_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn make_dialog() -> ClientPickerDialog {
        let enabled = Rc::new(RefCell::new(ClientId::iter().collect::<HashSet<_>>()));
        let needs_reload = Rc::new(RefCell::new(false));
        ClientPickerDialog::new(enabled, needs_reload)
    }

    fn first_hotkey_client() -> (ClientId, char) {
        let client = ClientId::iter()
            .find(|client| client_ui::hotkey(*client).is_some())
            .expect("catalog should expose at least one picker hotkey");
        (client, hotkey(client))
    }

    #[test]
    fn source_picker_plain_hotkey_char_filters_instead_of_toggling() {
        let (client, key_char) = first_hotkey_client();
        let mut dialog = make_dialog();

        let result = dialog.handle_key(key(KeyCode::Char(key_char)));

        assert!(matches!(result, DialogResult::Handled));
        assert_eq!(dialog.filter, key_char.to_string());
        assert!(dialog.enabled.borrow().contains(&client));
    }

    #[test]
    fn source_picker_alt_hotkey_toggles() {
        let (client, key_char) = first_hotkey_client();
        let mut dialog = make_dialog();

        let result = dialog.handle_key(alt_key(key_char));

        assert!(matches!(result, DialogResult::NeedsReload));
        assert!(!dialog.enabled.borrow().contains(&client));
        assert!(*dialog.needs_reload.borrow());
    }

    #[test]
    fn source_picker_enter_toggles_filtered_selection() {
        let mut dialog = make_dialog();
        dialog.filter = display_name(dialog.sources[0]).to_lowercase();
        dialog.rebuild_filter();
        let source_idx = dialog.filtered_indices[dialog.selected];
        let client = dialog.sources[source_idx];

        let result = dialog.handle_key(key(KeyCode::Enter));

        assert!(matches!(result, DialogResult::NeedsReload));
        assert!(!dialog.enabled.borrow().contains(&client));
    }

    #[test]
    fn source_picker_backspace_updates_filter() {
        let mut dialog = make_dialog();
        dialog.handle_key(key(KeyCode::Char('a')));
        dialog.handle_key(key(KeyCode::Char('m')));

        let result = dialog.handle_key(key(KeyCode::Backspace));

        assert!(matches!(result, DialogResult::Handled));
        assert_eq!(dialog.filter, "a");
        assert!(dialog.filtered_indices.len() <= dialog.sources.len());
    }

    #[test]
    fn source_picker_mouse_hitbox_toggles_visible_row() {
        let mut dialog = make_dialog();
        let area = Rect::new(0, 0, 50, 18);
        let list = source_picker_areas(area).list;
        let client = dialog.client_at(area, list.x, list.y).unwrap();

        let result = dialog.handle_mouse(click(list.x, list.y), area);

        assert!(matches!(result, DialogResult::NeedsReload));
        assert!(!dialog.enabled.borrow().contains(&client));
    }

    #[test]
    fn source_picker_mouse_hitbox_respects_filter_scroll() {
        let mut dialog = make_dialog();
        let area = Rect::new(0, 0, 50, 10);
        let list = source_picker_areas(area).list;
        dialog.selected = 5;
        let expected = dialog.client_at(area, list.x, list.y).unwrap();

        let result = dialog.handle_mouse(click(list.x, list.y), area);

        assert!(matches!(result, DialogResult::NeedsReload));
        assert_eq!(dialog.selected, 2);
        assert!(!dialog.enabled.borrow().contains(&expected));
    }

    #[test]
    fn source_picker_mouse_outside_rows_does_not_toggle() {
        let mut dialog = make_dialog();
        let area = Rect::new(0, 0, 50, 18);
        let divider = source_picker_areas(area).divider;
        let enabled_before = dialog.enabled.borrow().clone();

        let result = dialog.handle_mouse(click(divider.x, divider.y), area);

        assert!(matches!(
            result,
            DialogResult::Ignored("click outside rows")
        ));
        assert_eq!(*dialog.enabled.borrow(), enabled_before);
    }
}
