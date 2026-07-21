mod agents;
mod bar_chart;
mod daily;
mod daily_profile;
pub mod dialog;
mod donut;
mod footer;
mod header;
mod hourly;
mod hourly_profile;
mod loading;
mod model_usage_layout;
mod models;
mod overview;
mod overview_snapshot;
mod period;
mod radar;
mod sessions;
pub mod spinner;
mod stats;
mod table_layout;
mod usage;
mod usage_profile;
mod view_footer;
pub(crate) mod widgets;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::app::{App, Tab};
use crate::tui::view_state::ViewState;

pub(crate) fn render_with_state(frame: &mut Frame, app: &mut App, state: &mut ViewState) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    app.clear_click_areas();
    app.handle_resize(area.width, area.height);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .split(area);

    header::render(frame, app, chunks[0]);

    // A failed first scan without an installed generation gets an Oops page
    // instead of misleading empty tabs. The Usage tab is not a local-report
    // surface: it never shows the local-scan failure page.
    let cold_failed = !app.has_installed_generation() && app.data.error.is_some();
    if app.data.loading && !app.background_loading {
        render_loading(frame, app, chunks[1]);
    } else if app.background_loading && !app.has_installed_generation() {
        // Cold start: the first scan is still running and no cached
        // generation is installed, so there is nothing meaningful to show
        // yet — render the loading state instead of empty/zero tab states.
        render_loading(frame, app, chunks[1]);
    } else if cold_failed && app.current_tab != Tab::Usage {
        render_cold_failed(frame, app, chunks[1]);
    } else {
        render_current_tab(frame, app, state, chunks[1]);
    }

    view_footer::render(frame, app, state, chunks[2]);

    if app.dialog_stack.is_active() {
        app.dialog_stack.render(frame, area);
    }
}

fn render_current_tab(frame: &mut Frame, app: &mut App, state: &mut ViewState, area: Rect) {
    match app.current_tab {
        Tab::Overview => overview_snapshot::render(frame, app, area),
        Tab::Models => models::render(frame, app, area),
        Tab::Agents => agents::render(frame, app, area),
        Tab::Daily => render_daily(frame, app, state, area),
        Tab::Hourly => hourly::render(frame, app, area),
        Tab::Monthly => period::render_monthly(frame, app, area),
        Tab::Weekly => period::render_weekly(frame, app, area),
        Tab::Stats => stats::render(frame, app, area),
        Tab::Usage => usage::render(frame, app, area),
        Tab::Sessions => sessions::render(frame, app, state, area),
    }
}

fn render_cold_failed(frame: &mut Frame, app: &App, area: Rect) {
    let diagnostic = app.data.error.as_deref().unwrap_or("unknown error");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        Line::from(Span::styled(
            "Could not load local reports",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            diagnostic.to_string(),
            Style::default().fg(app.theme.muted),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("[r] Retry", Style::default().fg(Color::Yellow)),
            Span::raw("    "),
            Span::styled("[q] Quit", Style::default().fg(app.theme.muted)),
        ]),
    ];
    let paragraph = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });

    // Constrain the text block so long diagnostics wrap into a readable
    // centered column instead of one clipped edge-to-edge line.
    let content_width = inner.width.saturating_sub(4).min(100).max(20);
    let content_height = (wrapped_line_count(diagnostic, content_width as usize) + 4) as u16;
    let content = Rect {
        x: inner.x + inner.width.saturating_sub(content_width) / 2,
        y: inner.y + inner.height.saturating_sub(content_height) / 2,
        width: content_width,
        height: content_height.min(inner.height),
    };
    frame.render_widget(paragraph, content);
}

/// Rough word-wrap line estimate for centering the Oops text block (ratatui
/// 0.29 does not expose its wrapped-height measurement).
fn wrapped_line_count(text: &str, width: usize) -> usize {
    debug_assert!(width > 0);
    let mut lines = 1;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        let separator = usize::from(col > 0);
        if col + separator + word_len <= width {
            col += separator + word_len;
        } else if word_len <= width {
            lines += 1;
            col = word_len;
        } else {
            // Word longer than a line: ratatui hard-splits it across rows.
            let available = width.saturating_sub(col + separator);
            let overflow = word_len - available;
            lines += 1 + overflow / width;
            col = overflow % width;
        }
    }
    lines
}

fn render_daily(frame: &mut Frame, app: &mut App, state: &mut ViewState, area: Rect) {
    if app.is_daily_detail_active() || !state.daily_profile_active() {
        daily::render(frame, app, area);
    } else {
        daily_profile::render(frame, app, state, area);
    }
}

fn render_loading(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    loading::render(
        frame,
        app,
        inner,
        spinner::get_phase_message("parsing-sources"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{ProjectionBackend, TuiConfig};
    use crate::tui::view_state::ViewState;
    use ratatui::{backend::TestBackend, Terminal};

    fn make_app() -> App {
        let config = TuiConfig {
            theme: Some("blue".to_string()),
            refresh: 0,
            no_refresh: false,
            home_dir: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        App::new_with_cached_data(config, None).unwrap()
    }

    fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer
            .content()
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    fn render_screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut state = ViewState::default();
        terminal
            .draw(|frame| render_with_state(frame, app, &mut state))
            .unwrap();
        buffer_lines(&terminal)
    }

    #[test]
    fn cold_start_scan_renders_loading_instead_of_empty_states() {
        let mut app = make_app();
        app.set_background_loading(true);

        let screen = render_screen(&mut app, 120, 32).join("\n");

        assert!(screen.contains("Scanning session data..."));
        assert!(screen.contains('~'), "fish pond should render: {screen}");
        assert!(screen.contains('°'), "fish pond should render: {screen}");
        assert!(!screen.contains("No session data available"));
        assert!(!screen.contains("Total Tokens"));
    }

    #[test]
    fn cramped_terminal_cold_start_shows_spinner_without_pond() {
        let mut app = make_app();
        app.set_background_loading(true);

        let screen = render_screen(&mut app, 40, 12).join("\n");

        assert!(screen.contains("Scanning session data..."), "{screen}");
        assert!(!screen.contains('°'), "pond must degrade away: {screen}");
    }

    #[test]
    fn cold_start_failure_renders_oops_with_wrapped_diagnostic_and_retry_hints() {
        let mut app = make_app();
        let diagnostic = "injected cold failure with a deliberately long message that must wrap onto multiple lines inside the content area";
        app.set_error(Some(diagnostic.to_string()));

        let lines = render_screen(&mut app, 120, 32);
        let screen = lines.join("\n");
        let footer = lines[lines.len() - 5..].join("\n");

        assert!(screen.contains("Could not load local reports"), "{screen}");
        assert!(screen.contains("[r] Retry"), "{screen}");
        assert!(screen.contains("[q] Quit"), "{screen}");
        // the misleading empty/zero tab states stay behind the Oops page
        assert!(!screen.contains("No session data available"), "{screen}");
        assert!(!screen.contains("Total Tokens"), "{screen}");
        // long diagnostics wrap across rows instead of clipping at the edge
        let head_row = lines
            .iter()
            .position(|line| line.contains("injected cold failure"));
        let tail_row = lines.iter().position(|line| line.contains("content area"));
        assert!(
            head_row.is_some() && tail_row.is_some() && head_row != tail_row,
            "diagnostic must wrap onto multiple rows: {screen}"
        );
        // the failure is carried by the Oops page alone, never the footer
        assert!(!footer.contains("injected cold failure"), "{footer}");
    }

    #[test]
    fn cold_start_failure_keeps_usage_tab_untouched() {
        let mut app = make_app();
        app.set_error(Some("injected cold failure".to_string()));
        app.current_tab = Tab::Usage;

        let lines = render_screen(&mut app, 120, 32);
        let screen = lines.join("\n");
        let footer = lines[lines.len() - 5..].join("\n");

        assert!(
            !screen.contains("Could not load local reports"),
            "Usage tab is not a local-report surface: {screen}"
        );
        assert!(screen.contains("subscription"), "{screen}");
        assert!(
            !footer.contains("injected cold failure"),
            "local-scan failures never leak into the footer: {footer}"
        );
    }

    #[test]
    fn cold_start_failure_retry_key_queues_a_background_reload() {
        let mut app = make_app();
        app.set_error(Some("injected cold failure".to_string()));

        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ));

        assert!(app.needs_reload);
        assert!(app.reload_force);
    }

    #[test]
    fn background_refresh_with_installed_generation_keeps_content_visible() {
        let mut app = make_app();
        app.install_tui_snapshot(
            crate::tui::data::UsageData {
                total_tokens: 77,
                ..Default::default()
            },
            Vec::new(),
            Default::default(),
            ProjectionBackend::Memory(tokscale_core::TuiAcc::new()),
            tokscale_core::GroupBy::Model,
        );
        app.set_background_loading(true);

        let screen = render_screen(&mut app, 120, 32).join("\n");

        assert!(
            screen.contains("Total Tokens"),
            "installed generation must keep the tab content visible: {screen}"
        );
        assert!(
            screen.contains("Refreshing cached data in background..."),
            "{screen}"
        );
    }
}
