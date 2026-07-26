use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use tokenx_engine::ClientId;

use crate::tui::interaction::{InteractionOutcome, ListInteraction};
use crate::tui::themes::Theme;

use super::{DialogContent, DialogResult, UiCommand};

/// TUI dialog that lets the user toggle which clients are included in reports.
/// Edits stay in a draft until Enter commits them; closing with Esc or by
/// clicking outside discards the draft.
pub struct ClientPickerDialog {
    /// Every selectable filter in the same order they appear on screen.
    /// Retains the accepted catalog's canonical order.
    clients: Vec<ClientId>,
    draft_enabled: HashSet<ClientId>,
    selected: usize,
    filter: String,
    /// Indices into `clients` that match the current type-to-filter
    /// substring. `selected` indexes into this vec, not into `clients`.
    filtered_indices: Vec<usize>,
    last_error: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClientPickerAreas {
    filter: Rect,
    divider: Rect,
    list: Rect,
    hint: Rect,
}

impl ClientPickerDialog {
    pub fn new(clients: Vec<ClientId>, enabled: HashSet<ClientId>) -> Self {
        let filtered_indices: Vec<usize> = (0..clients.len()).collect();
        Self {
            clients,
            draft_enabled: enabled,
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

    /// Toggle the currently highlighted client in the dialog-local draft.
    /// The draft may be empty; the non-empty invariant is enforced only when
    /// the user commits it.
    fn toggle_selected(&mut self) -> InteractionOutcome {
        if let Some(&idx) = self.filtered_indices.get(self.selected) {
            self.toggle(self.clients[idx])
        } else {
            InteractionOutcome::Ignored("empty filtered list")
        }
    }

    fn toggle(&mut self, client: ClientId) -> InteractionOutcome {
        if !self.clients.contains(&client) {
            return InteractionOutcome::Ignored("client outside loaded universe");
        }

        if !self.draft_enabled.remove(&client) {
            self.draft_enabled.insert(client);
        }
        self.last_error = None;
        InteractionOutcome::Handled
    }

    /// Invert every row matched by the current filter. With an empty filter,
    /// `filtered_indices` contains the complete loaded client universe.
    fn invert_filtered(&mut self) -> InteractionOutcome {
        for &idx in &self.filtered_indices {
            let client = self.clients[idx];
            if !self.draft_enabled.remove(&client) {
                self.draft_enabled.insert(client);
            }
        }
        self.last_error = None;
        InteractionOutcome::Handled
    }

    fn commit(&mut self) -> DialogResult {
        if self.draft_enabled.is_empty() {
            self.last_error = Some("Select at least one client");
            return DialogResult::Handled;
        }

        DialogResult::Submit(UiCommand::ProjectClients(self.draft_enabled.clone()))
    }

    fn rebuild_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        if needle.is_empty() {
            self.filtered_indices = (0..self.clients.len()).collect();
        } else {
            self.filtered_indices = self
                .clients
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
        let areas = client_picker_areas(area);
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
        let client_idx = *self.filtered_indices.get(flat_idx)?;
        Some((flat_idx, self.clients[client_idx]))
    }

    #[cfg(test)]
    fn client_at(&self, area: Rect, column: u16, row: u16) -> Option<ClientId> {
        self.filtered_row_at(area, column, row)
            .map(|(_, client)| client)
    }
}

fn client_picker_areas(area: Rect) -> ClientPickerAreas {
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

    ClientPickerAreas {
        filter: rows[0],
        divider: rows[1],
        list: rows[2],
        hint: rows[3],
    }
}

fn client_picker_hint(width: u16) -> &'static str {
    if width >= 43 {
        "↑↓ • Space • * invert matches • Enter • Esc"
    } else if width >= 38 {
        "* invert matches • Space • Enter • Esc"
    } else {
        "* invert matches"
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
            .border_style(Style::default().fg(theme.chrome.focus));
        frame.render_widget(block, area);

        let rows = client_picker_areas(area);

        let filter_text = if self.filter.is_empty() {
            Span::styled(
                "Type to filter...",
                Style::default().fg(theme.text.secondary),
            )
        } else {
            Span::styled(&self.filter, Style::default().fg(theme.text.primary))
        };
        let filter_line = Paragraph::new(Line::from(vec![
            Span::styled("Filter: ", Style::default().fg(theme.chrome.focus)),
            filter_text,
        ]));
        frame.render_widget(filter_line, rows.filter);

        let divider = Paragraph::new("-".repeat(rows.divider.width as usize))
            .style(Style::default().fg(theme.chrome.border));
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

            let client = self.clients[idx];
            let is_selected = flat_idx == self.selected;
            let is_enabled = self.draft_enabled.contains(&client);

            let checkbox = if is_enabled { "[●]" } else { "[ ]" };
            let name = display_name(client);

            let usable = list_area.width.saturating_sub(4) as usize;
            let left = format!("{} {}", checkbox, name);
            let padding = usable.saturating_sub(left.chars().count());

            let base_style = if is_selected {
                theme.selection_style()
            } else if is_enabled {
                Style::default().fg(theme.text.primary)
            } else {
                Style::default().fg(theme.text.secondary)
            };

            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("  {}", left), base_style),
                Span::styled(" ".repeat(padding), base_style),
            ])));
        }

        if items.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "  No results",
                Style::default().fg(theme.text.secondary),
            ))));
        }

        frame.render_widget(List::new(items), list_area);

        let hint_text = self
            .last_error
            .unwrap_or_else(|| client_picker_hint(rows.hint.width));
        let hint_style = if self.last_error.is_some() {
            Style::default().fg(theme.status.warning)
        } else {
            Style::default().fg(theme.text.secondary)
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
            KeyCode::Enter => self.commit(),
            KeyCode::Char(' ') => self.toggle_selected().into(),
            KeyCode::Char('*') => self.invert_filtered().into(),
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild_filter();
                self.last_error = None;
                DialogResult::Handled
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.rebuild_filter();
                self.last_error = None;
                DialogResult::Handled
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
    client.display_name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{backend::TestBackend, Terminal};

    use crate::tui::themes::ThemeName;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
        let clients = ClientId::iter().collect::<Vec<_>>();
        let enabled = clients.iter().copied().collect();
        ClientPickerDialog::new(clients, enabled)
    }

    fn make_dialog_for(clients: Vec<ClientId>) -> ClientPickerDialog {
        let enabled = clients.iter().copied().collect();
        ClientPickerDialog::new(clients, enabled)
    }

    fn render_symbols(dialog: &ClientPickerDialog, area: Rect) -> String {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        let theme = Theme::from_name(ThemeName::Blue);
        let frame = terminal
            .draw(|frame| dialog.render(frame, area, &theme))
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
    fn client_picker_lists_exactly_the_loaded_universe() {
        let dialog = make_dialog();
        let expected = ClientId::iter().collect::<Vec<_>>();

        assert_eq!(dialog.clients, expected);
        assert!(dialog.clients.contains(&ClientId::Claude));
    }

    #[test]
    fn client_picker_plain_char_filters_without_toggling_selection() {
        let mut dialog = make_dialog();
        let draft_before = dialog.draft_enabled.clone();

        let result = dialog.handle_key(key(KeyCode::Char('c')));

        assert!(matches!(result, DialogResult::Handled));
        assert_eq!(dialog.filter, "c");
        assert_eq!(dialog.draft_enabled, draft_before);
    }

    #[test]
    fn client_picker_enter_commits_the_draft_and_closes() {
        let mut dialog = make_dialog();
        dialog.filter = display_name(dialog.clients[0]).to_lowercase();
        dialog.rebuild_filter();
        let client_idx = dialog.filtered_indices[dialog.selected];
        let client = dialog.clients[client_idx];
        assert!(matches!(
            dialog.handle_key(key(KeyCode::Char(' '))),
            DialogResult::Handled
        ));

        let result = dialog.handle_key(key(KeyCode::Enter));

        let DialogResult::Submit(UiCommand::ProjectClients(selection)) = result else {
            panic!("Enter must submit the client draft");
        };
        assert!(!selection.contains(&client));
    }

    #[test]
    fn client_picker_inverts_only_filtered_matches() {
        let mut dialog =
            make_dialog_for(vec![ClientId::Claude, ClientId::Codex, ClientId::OpenCode]);
        for c in "code".chars() {
            dialog.handle_key(key(KeyCode::Char(c)));
        }

        let result = dialog.handle_key(key(KeyCode::Char('*')));

        assert!(matches!(result, DialogResult::Handled));
        assert_eq!(dialog.filter, "code");
        assert_eq!(dialog.filtered_indices.len(), 2);
        assert!(dialog.draft_enabled.contains(&ClientId::Claude));
        assert!(!dialog.draft_enabled.contains(&ClientId::Codex));
        assert!(!dialog.draft_enabled.contains(&ClientId::OpenCode));
    }

    #[test]
    fn client_picker_empty_filter_inverts_the_entire_universe() {
        let mut dialog = make_dialog_for(vec![ClientId::Claude, ClientId::Codex]);

        assert!(matches!(
            dialog.handle_key(key(KeyCode::Char('*'))),
            DialogResult::Handled
        ));
        assert!(dialog.draft_enabled.is_empty());

        dialog.handle_key(key(KeyCode::Char('*')));
        assert_eq!(
            dialog.draft_enabled,
            HashSet::from([ClientId::Claude, ClientId::Codex])
        );
    }

    #[test]
    fn client_picker_space_can_leave_an_empty_draft() {
        let mut dialog = make_dialog_for(vec![ClientId::Claude]);

        let result = dialog.handle_key(key(KeyCode::Char(' ')));

        assert!(matches!(result, DialogResult::Handled));
        assert!(dialog.draft_enabled.is_empty());
        assert!(dialog.last_error.is_none());
    }

    #[test]
    fn client_picker_enter_rejects_an_empty_draft_without_committing() {
        let mut dialog = make_dialog_for(vec![ClientId::Claude]);
        dialog.handle_key(key(KeyCode::Char(' ')));

        let result = dialog.handle_key(key(KeyCode::Enter));

        assert!(matches!(result, DialogResult::Handled));
        assert_eq!(dialog.last_error, Some("Select at least one client"));
    }

    #[test]
    fn client_picker_hint_keeps_invert_matches_visible_at_narrow_width() {
        let dialog = make_dialog_for(vec![ClientId::Claude]);

        let rendered = render_symbols(&dialog, Rect::new(0, 0, 42, 18));

        assert!(rendered.contains("* invert matches • Space • Enter • Esc"));
    }

    #[test]
    fn client_picker_escape_discards_the_draft() {
        let mut dialog = make_dialog();
        let client = dialog.clients[0];
        dialog.handle_key(key(KeyCode::Char(' ')));

        let result = dialog.handle_key(key(KeyCode::Esc));

        assert!(matches!(result, DialogResult::Close));
        assert!(!dialog.draft_enabled.contains(&client));
    }

    #[test]
    fn client_picker_backspace_updates_filter() {
        let mut dialog = make_dialog();
        dialog.handle_key(key(KeyCode::Char('a')));
        dialog.handle_key(key(KeyCode::Char('m')));

        let result = dialog.handle_key(key(KeyCode::Backspace));

        assert!(matches!(result, DialogResult::Handled));
        assert_eq!(dialog.filter, "a");
        assert!(dialog.filtered_indices.len() <= dialog.clients.len());
    }

    #[test]
    fn client_picker_mouse_hitbox_toggles_visible_row() {
        let mut dialog = make_dialog();
        let area = Rect::new(0, 0, 50, 18);
        let list = client_picker_areas(area).list;
        let client = dialog.client_at(area, list.x, list.y).unwrap();

        let result = dialog.handle_mouse(click(list.x, list.y), area);

        assert!(matches!(result, DialogResult::Handled));
        assert!(!dialog.draft_enabled.contains(&client));
    }

    #[test]
    fn client_picker_mouse_hitbox_respects_filter_scroll() {
        let mut dialog = make_dialog();
        let area = Rect::new(0, 0, 50, 10);
        let list = client_picker_areas(area).list;
        dialog.selected = 5;
        let expected = dialog.client_at(area, list.x, list.y).unwrap();

        let result = dialog.handle_mouse(click(list.x, list.y), area);

        assert!(matches!(result, DialogResult::Handled));
        assert_eq!(dialog.selected, 2);
        assert!(!dialog.draft_enabled.contains(&expected));
    }

    #[test]
    fn client_picker_mouse_outside_rows_does_not_toggle() {
        let mut dialog = make_dialog();
        let area = Rect::new(0, 0, 50, 18);
        let divider = client_picker_areas(area).divider;
        let enabled_before = dialog.draft_enabled.clone();

        let result = dialog.handle_mouse(click(divider.x, divider.y), area);

        assert!(matches!(
            result,
            DialogResult::Ignored("click outside rows")
        ));
        assert_eq!(dialog.draft_enabled, enabled_before);
    }
}
