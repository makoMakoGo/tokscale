use std::cmp::Ordering;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};

use super::app::{App, SortDirection, SortField, Tab};
use super::interaction::{ListInteraction, MoveCommand, TextViewport, WrapMode};
use super::session_data::{self, SessionEntry, SourceSummary};

#[derive(Debug, Default)]
pub(crate) struct ViewState {
    daily_profile: bool,
    daily_profile_viewport: TextViewport,
    daily_profile_total_lines: usize,
    selected_session_source: Option<String>,
    session_sources: ListInteraction,
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
            self.move_session_selection(command);
            return true;
        }

        match key.code {
            KeyCode::Enter if !self.session_detail_active() => {
                if let Some(source) = self
                    .source_rows(app)
                    .get(self.session_sources.selected)
                    .map(|row| row.source.clone())
                {
                    self.selected_session_source = Some(source);
                    self.session_details = ListInteraction::default();
                }
                true
            }
            KeyCode::Esc | KeyCode::Backspace if self.session_detail_active() => {
                self.selected_session_source = None;
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

        self.move_session_selection(command);
        true
    }

    fn move_session_selection(&mut self, command: MoveCommand) {
        let detail_active = self.session_detail_active();
        let len = if detail_active {
            self.session_count()
        } else {
            self.source_count()
        };
        let interaction = if detail_active {
            &mut self.session_details
        } else {
            &mut self.session_sources
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
        self.selected_session_source.is_some()
    }

    pub(crate) fn selected_session_source(&self) -> Option<&str> {
        self.selected_session_source.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn select_session_source_for_test(&mut self, source: &str) {
        self.selected_session_source = Some(source.to_string());
    }

    pub(crate) fn source_count(&self) -> usize {
        session_data::snapshot().source_count()
    }

    pub(crate) fn session_count(&self) -> usize {
        let snapshot = session_data::snapshot();
        self.selected_session_source.as_deref().map_or_else(
            || snapshot.session_count(),
            |source| snapshot.session_count_for_source(source),
        )
    }

    pub(crate) fn source_rows(&self, app: &App) -> Vec<SourceSummary> {
        let snapshot = session_data::snapshot();
        let mut rows = snapshot.source_summaries().to_vec();
        rows.sort_by(|left, right| {
            let ordering = match app.sort_field {
                SortField::Date => left.last_seen.cmp(&right.last_seen),
                SortField::Tokens => left.session_count.cmp(&right.session_count),
                SortField::Cost => left.space_bytes.cmp(&right.space_bytes),
            };
            apply_direction(ordering, app.sort_direction)
                .then_with(|| left.source.cmp(&right.source))
        });
        rows
    }

    pub(crate) fn session_rows(&self, app: &App) -> Vec<SessionEntry> {
        let Some(source) = self.selected_session_source.as_deref() else {
            return Vec::new();
        };
        let snapshot = session_data::snapshot();
        let mut rows = snapshot.sessions_for_source(source);
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

    pub(crate) fn set_source_viewport(&mut self, visible: usize, len: usize) {
        self.session_sources.set_visible(visible, len);
    }

    pub(crate) fn set_detail_viewport(&mut self, visible: usize, len: usize) {
        self.session_details.set_visible(visible, len);
    }

    pub(crate) fn source_selected(&self) -> usize {
        self.session_sources.selected
    }

    pub(crate) fn detail_selected(&self) -> usize {
        self.session_details.selected
    }

    pub(crate) fn source_scroll(&self) -> usize {
        self.session_sources.scroll
    }

    pub(crate) fn detail_scroll(&self) -> usize {
        self.session_details.scroll
    }

    pub(crate) fn source_visible_range(&self, len: usize) -> Range<usize> {
        self.session_sources.visible_range(len)
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
