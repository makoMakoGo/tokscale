use std::cmp::Ordering;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};

use super::app::{App, SortDirection, SortField, Tab};
use super::interaction::{ListInteraction, MoveCommand, TextViewport, WrapMode};
use super::session_data::{ClientSummary, SessionEntry};

#[derive(Debug, Default)]
pub(crate) struct ViewState {
    daily_profile: bool,
    daily_profile_viewport: TextViewport,
    daily_profile_total_lines: usize,
    selected_session_client: Option<String>,
    session_clients: ListInteraction,
    session_details: ListInteraction,
}

impl ViewState {
    pub(crate) fn handle_key(&mut self, app: &App, key: &KeyEvent) -> bool {
        if app.dialog_stack.is_active() {
            return false;
        }

        if app.current_tab == Tab::Daily && !app.is_daily_detail_active() {
            if key.code == KeyCode::Char('v') {
                self.daily_profile = !self.daily_profile;
                return true;
            }

            if self.daily_profile {
                if let Some(command) = move_command(key.code) {
                    self.move_daily_profile(command);
                    return true;
                }
                if matches!(key.code, KeyCode::Enter | KeyCode::Char('j')) {
                    return true;
                }
            }
        }

        if app.current_tab != Tab::Sessions {
            return false;
        }

        if let Some(command) = move_command(key.code) {
            self.move_session_selection(app, command);
            return true;
        }

        match key.code {
            KeyCode::Enter if !self.session_detail_active() => {
                if let Some(client) = self
                    .client_rows(app)
                    .get(self.session_clients.selected)
                    .map(|row| row.client.clone())
                {
                    self.selected_session_client = Some(client);
                    self.session_details = ListInteraction::default();
                }
                true
            }
            KeyCode::Esc | KeyCode::Backspace if self.session_detail_active() => {
                self.selected_session_client = None;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn handle_mouse(&mut self, app: &App, event: &MouseEvent) -> bool {
        if app.dialog_stack.is_active() {
            return false;
        }

        let Some(command) = wheel_move_command(event.kind) else {
            return false;
        };

        if app.current_tab == Tab::Daily && !app.is_daily_detail_active() && self.daily_profile {
            self.move_daily_profile(command);
            return true;
        }

        if app.current_tab != Tab::Sessions {
            return false;
        }

        self.move_session_selection(app, command);
        true
    }

    fn move_session_selection(&mut self, app: &App, command: MoveCommand) {
        let detail_active = self.session_detail_active();
        let len = if detail_active {
            self.session_count(app)
        } else {
            self.client_count(app)
        };
        let interaction = if detail_active {
            &mut self.session_details
        } else {
            &mut self.session_clients
        };
        interaction.apply_move(command, len, WrapMode::Wrap);
    }

    fn move_daily_profile(&mut self, command: MoveCommand) {
        self.daily_profile_viewport
            .apply_move(command, self.daily_profile_total_lines);
    }

    pub(crate) fn daily_profile_active(&self) -> bool {
        self.daily_profile
    }

    pub(crate) fn set_daily_profile_text_viewport(&mut self, visible: usize, total_lines: usize) {
        self.daily_profile_total_lines = total_lines;
        self.daily_profile_viewport
            .set_visible(visible, total_lines);
    }

    pub(crate) fn daily_profile_text_visible_range(&self) -> Range<usize> {
        self.daily_profile_viewport
            .visible_range(self.daily_profile_total_lines)
    }

    pub(crate) fn daily_profile_scroll(&self) -> usize {
        self.daily_profile_viewport.scroll
    }

    pub(crate) fn session_detail_active(&self) -> bool {
        self.selected_session_client.is_some()
    }

    pub(crate) fn selected_session_client(&self) -> Option<&str> {
        self.selected_session_client.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn select_session_client_for_test(&mut self, client: &str) {
        self.selected_session_client = Some(client.to_string());
    }

    pub(crate) fn client_count(&self, app: &App) -> usize {
        app.session_snapshot
            .client_summaries()
            .iter()
            .filter(|summary| app.is_client_selected(&summary.client))
            .count()
    }

    pub(crate) fn session_count(&self, app: &App) -> usize {
        let snapshot = &app.session_snapshot;
        self.selected_session_client.as_deref().map_or_else(
            || {
                snapshot
                    .client_summaries()
                    .iter()
                    .filter(|summary| app.is_client_selected(&summary.client))
                    .map(|summary| summary.session_count)
                    .sum()
            },
            |client| {
                if app.is_client_selected(client) {
                    snapshot.session_count_for_client(client)
                } else {
                    0
                }
            },
        )
    }

    pub(crate) fn client_rows(&self, app: &App) -> Vec<ClientSummary> {
        let mut rows = app
            .session_snapshot
            .client_summaries()
            .iter()
            .filter(|summary| app.is_client_selected(&summary.client))
            .cloned()
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            let ordering = match app.sort_field {
                SortField::Date => left.last_seen.cmp(&right.last_seen),
                SortField::Tokens => left.session_count.cmp(&right.session_count),
                SortField::Cost => left.space_bytes.cmp(&right.space_bytes),
            };
            apply_direction(ordering, app.sort_direction)
                .then_with(|| left.client.cmp(&right.client))
        });
        rows
    }

    pub(crate) fn session_rows<'a>(&self, app: &'a App) -> Vec<&'a SessionEntry> {
        let Some(client) = self.selected_session_client.as_deref() else {
            return Vec::new();
        };
        if !app.is_client_selected(client) {
            return Vec::new();
        }
        let mut rows = app
            .session_snapshot
            .session_refs_for_client(client)
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            let ordering = match app.sort_field {
                SortField::Date => left.last_seen.cmp(&right.last_seen),
                SortField::Tokens => left.tokens.total().cmp(&right.tokens.total()),
                SortField::Cost => left.cost.total_cmp(&right.cost),
            };
            apply_direction(ordering, app.sort_direction)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        rows
    }

    pub(crate) fn reconcile_session_snapshot(&mut self, app: &App) {
        if self
            .selected_session_client
            .as_deref()
            .is_some_and(|selected| {
                !app.session_snapshot
                    .client_summaries()
                    .iter()
                    .any(|summary| summary.client == selected)
                    || !app.is_client_selected(selected)
            })
        {
            self.selected_session_client = None;
            self.session_details = ListInteraction::default();
        }

        self.session_clients.clamp(self.client_count(app));
        self.session_details.clamp(self.session_count(app));
    }

    pub(crate) fn set_client_viewport(&mut self, visible: usize, len: usize) {
        self.session_clients.set_visible(visible, len);
    }

    pub(crate) fn set_detail_viewport(&mut self, visible: usize, len: usize) {
        self.session_details.set_visible(visible, len);
    }

    pub(crate) fn client_selected(&self) -> usize {
        self.session_clients.selected
    }

    pub(crate) fn detail_selected(&self) -> usize {
        self.session_details.selected
    }

    pub(crate) fn client_scroll(&self) -> usize {
        self.session_clients.scroll
    }

    pub(crate) fn detail_scroll(&self) -> usize {
        self.session_details.scroll
    }

    pub(crate) fn client_visible_range(&self, len: usize) -> Range<usize> {
        self.session_clients.visible_range(len)
    }

    pub(crate) fn detail_visible_range(&self, len: usize) -> Range<usize> {
        self.session_details.visible_range(len)
    }
}

fn apply_direction(ordering: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Ascending => ordering,
        SortDirection::Descending => ordering.reverse(),
    }
}

fn move_command(code: KeyCode) -> Option<MoveCommand> {
    match code {
        KeyCode::Up => Some(MoveCommand::Up),
        KeyCode::Down => Some(MoveCommand::Down),
        KeyCode::PageUp => Some(MoveCommand::PageUp),
        KeyCode::PageDown => Some(MoveCommand::PageDown),
        KeyCode::Home => Some(MoveCommand::Home),
        KeyCode::End => Some(MoveCommand::End),
        _ => None,
    }
}

fn wheel_move_command(kind: MouseEventKind) -> Option<MoveCommand> {
    match kind {
        MouseEventKind::ScrollUp => Some(MoveCommand::Up),
        MouseEventKind::ScrollDown => Some(MoveCommand::Down),
        _ => None,
    }
}
