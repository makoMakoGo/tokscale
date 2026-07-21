use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::tui::app::App;

/// Braille spinner frames shared by every content-area loading state
/// (cold-start scan, subscription fetch, ...).
const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Centered single-line "spinner + message" loading state, matching the
/// ecosystem convention (gh-dash et al.) of one quiet spinner in the
/// content area while chrome (header/footer) keeps rendering.
pub(super) fn render(frame: &mut Frame, app: &App, area: Rect, message: &str) {
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let glyph = SPINNER_FRAMES[app.spinner_frame % SPINNER_FRAMES.len()];
    let line = Line::from(vec![
        Span::styled(glyph.to_string(), Style::default().fg(app.theme.accent)),
        Span::raw(" "),
        Span::styled(message.to_string(), Style::default().fg(app.theme.muted)),
    ]);
    let paragraph = Paragraph::new(line).alignment(Alignment::Center);
    frame.render_widget(paragraph, center);
}
