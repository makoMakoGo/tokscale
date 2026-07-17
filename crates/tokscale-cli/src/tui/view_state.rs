use std::cmp::Ordering;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent};

use super::app::{App, SortDirection, SortField, Tab};
use super::interaction::{ListInteraction, MoveCommand, WrapMode};
use super::session_data::{self, SessionEntry, SourceSummary};

#[derive(Debug, Default)]
pub(crate) struct ViewState {
    daily_profile: bool,
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

            if self.daily_profile
                && matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                        | KeyCode::Home
                        | KeyCode::End
                        | KeyCode::Enter
                        | KeyCode::Char('j')
                )
            {
                return true;
            }
        }

        if app.current_tab != Tab::Issues {
            return false;
        }

        if let Some(command) = move_command(key.code) {
            if self.session_detail_active() {
                let len = self.session_count();
                self.session_details
                    .apply_move(command, len, WrapMode::Wrap);
            } else {
                let len = self.source_count();
                self.session_sources
                    .apply_move(command, len, WrapMode::Wrap);
            }
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

    pub(crate) fn daily_profile_active(&self) -> bool {
        self.daily_profile
    }

    pub(crate) fn session_detail_active(&self) -> bool {
        self.selected_session_source.is_some()
    }

    pub(crate) fn selected_session_source(&self) -> Option<&str> {
        self.selected_session_source.as_deref()
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
