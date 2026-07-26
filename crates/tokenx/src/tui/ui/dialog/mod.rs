pub mod client_picker;
pub mod group_by_picker;
pub mod overlay;
pub mod stack;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{layout::Rect, Frame};
use std::collections::HashSet;
use tokenx_engine::{ClientId, GroupBy};

use crate::tui::interaction::InteractionOutcome;
use crate::tui::themes::Theme;

pub use client_picker::ClientPickerDialog;
pub use group_by_picker::GroupByPickerDialog;
pub use stack::DialogStack;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    ProjectClients(HashSet<ClientId>),
    ProjectGroupBy(GroupBy),
}

/// Result of handling a dialog event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogResult {
    /// Event was consumed without a stack-level action
    Handled,
    /// Event was understood but could not apply
    Ignored(&'static str),
    /// Close the current dialog
    Close,
    /// Close the dialog and submit one typed application command.
    Submit(UiCommand),
}

impl From<InteractionOutcome> for DialogResult {
    fn from(outcome: InteractionOutcome) -> Self {
        match outcome {
            InteractionOutcome::Handled => DialogResult::Handled,
            InteractionOutcome::Ignored(reason) => DialogResult::Ignored(reason),
        }
    }
}

/// Trait for dialog content that can be rendered and handle events
pub trait DialogContent {
    /// Return the desired (width, height) for the dialog
    fn desired_size(&self, viewport: Rect) -> (u16, u16);

    /// Render the dialog content within the given area
    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme);

    /// Handle a key event, return the result
    fn handle_key(&mut self, _key: KeyEvent) -> DialogResult {
        DialogResult::Ignored("unhandled key")
    }

    /// Handle a mouse event, return the result
    fn handle_mouse(&mut self, _event: MouseEvent, _area: Rect) -> DialogResult {
        DialogResult::Ignored("unhandled mouse")
    }
}
