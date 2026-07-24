use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::tui::app::App;

/// Canonical message stem for the cold-start acquisition state.
pub(super) const SCANNING_LOCAL_DATA: &str = "Scanning local data";

/// Braille spinner frames shared by every content-area loading state
/// (cold-start scan, subscription fetch, ...).
const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Fish-pond loop: a fish leaps out of the water, flips, and splashes back.
/// Original artwork (no third-party assets). Rows are trimmed; the pond is
/// centered as a block by the caller.
const POND_FRAMES: [&[&str]; 6] = [
    &[
        "",
        "",
        " ~  ~  ~  ~  ~  ~  ~",
        "      >}}}°>",
        "  .                  .",
        "",
    ],
    &[
        "",
        "         >}>",
        " ~  ~  ~  ~  ~  ~  ~",
        "         }}°>",
        "        '    '",
        "",
    ],
    &[
        "",
        "       >}}}°>",
        " ~  ~  ~ . ~ ' ~  ~",
        "          '",
        "        .      .",
        "",
    ],
    &[
        "",
        "       <°{{{<",
        " ~  ~  ~ . ~ ' ~  ~",
        "          '",
        "        .      .",
        "",
    ],
    &[
        "",
        " ~  ~ . ~ ' ~ . ~  ~",
        "        *<{}*",
        "       '  *  '",
        "         ' '",
        "",
    ],
    &[
        "",
        " ~  ~  ~  ~  ~  ~  ~",
        "       <°{{{<",
        "     .          .",
        "",
        "",
    ],
];

/// Ten 100ms steps per loop: linger on the calm pond and the settling
/// ripples so the leap reads as an event, not a strobe.
const POND_TIMELINE: [usize; 10] = [0, 0, 1, 2, 3, 4, 5, 5, 5, 5];

const POND_WIDTH: u16 = 23;

/// Centered "spinner + message" loading state with the fish-pond animation
/// above it; cramped areas degrade to the spinner line alone.
pub(super) fn render(frame: &mut Frame, app: &App, area: Rect, message: &str) {
    let show_pond = area.width >= POND_WIDTH + 4 && area.height >= 12;
    let spinner = spinner_line(app, message);

    if !show_pond {
        let center = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Length(3),
                Constraint::Percentage(40),
            ])
            .split(area)[1];
        let paragraph = Paragraph::new(spinner).alignment(Alignment::Center);
        frame.render_widget(paragraph, center);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(9);
    let pond_frame = POND_FRAMES[POND_TIMELINE[app.spinner_frame % POND_TIMELINE.len()]];
    for row in pond_frame {
        lines.push(pond_line(app, row));
    }
    lines.push(Line::from(""));
    lines.push(spinner);

    let content_height = lines.len() as u16;
    let content = Rect {
        y: area.y + area.height.saturating_sub(content_height) / 2,
        height: content_height.min(area.height),
        ..area
    };
    let paragraph = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(paragraph, content);
}

fn spinner_line(app: &App, message: &str) -> Line<'static> {
    let glyph = SPINNER_FRAMES[app.spinner_frame % SPINNER_FRAMES.len()];
    Line::from(vec![
        Span::styled(glyph.to_string(), Style::default().fg(app.theme.accent)),
        Span::raw(" "),
        Span::styled(
            format!("{message}..."),
            Style::default().fg(app.theme.muted),
        ),
    ])
}

/// Colors one pond row by character role so the artwork follows the active
/// theme instead of hardcoded RGB. Rows are padded to a uniform width so
/// per-line centering keeps the whole block aligned.
fn pond_line(app: &App, row: &str) -> Line<'static> {
    let padded = format!("{row:<width$}", width = POND_WIDTH as usize);
    let spans = padded
        .chars()
        .map(|ch| match ch {
            ' ' => Span::raw(" "),
            '~' => Span::styled(
                ch.to_string(),
                Style::default().fg(app.theme.color(Color::Rgb(94, 174, 212))),
            ),
            '>' | '<' | '}' | '{' | '°' => {
                Span::styled(ch.to_string(), Style::default().fg(app.theme.accent))
            }
            '*' => Span::styled(ch.to_string(), Style::default().fg(app.theme.foreground)),
            _ => Span::styled(ch.to_string(), Style::default().fg(app.theme.muted)),
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}
