pub mod group_by_picker;
pub mod overlay;
pub mod source_picker;
pub mod stack;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{layout::Rect, Frame};

use crate::tui::interaction::InteractionOutcome;
use crate::tui::themes::Theme;

pub use group_by_picker::GroupByPickerDialog;
pub use source_picker::ClientPickerDialog;
pub use stack::DialogStack;

/// Result of handling a dialog event
pub enum DialogResult {
    /// Event was consumed without a stack-level action
    Handled,
    /// Event was understood but could not apply
    Ignored(&'static str),
    /// Close the current dialog
    Close,
    /// Dialog data changed and the app should reload after close
    NeedsReload,
    /// Replace the current dialog with a new one
    #[allow(dead_code)]
    Replace(Box<dyn DialogContent>),
}

impl From<InteractionOutcome> for DialogResult {
    fn from(outcome: InteractionOutcome) -> Self {
        match outcome {
            InteractionOutcome::Handled => DialogResult::Handled,
            InteractionOutcome::Ignored(reason) => DialogResult::Ignored(reason),
            InteractionOutcome::Close => DialogResult::Close,
            InteractionOutcome::NeedsReload => DialogResult::NeedsReload,
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
