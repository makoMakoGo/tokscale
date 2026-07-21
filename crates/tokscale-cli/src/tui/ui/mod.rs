mod achievements;
mod agents;
mod bar_chart;
mod daily;
mod daily_profile;
pub mod dialog;
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
mod portraits;
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
use unicode_width::UnicodeWidthStr;

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

    // Only local-report tabs project the installed generation. Subscription
    // Usage has an independent remote-fetch lifecycle and stays usable while
    // local acquisition is cold-loading or has failed.
    let local_generation_tab = app.current_tab.depends_on_local_generation();
    let cold_failed =
        local_generation_tab && !app.has_installed_generation() && app.data.error.is_some();
    if local_generation_tab && app.data.loading && !app.background_loading {
        render_loading(frame, app, chunks[1]);
    } else if local_generation_tab && app.background_loading && !app.has_installed_generation() {
        // Cold start: the first scan is still running and no cached
        // generation is installed, so there is nothing meaningful to show
        // yet — render the loading state instead of empty/zero tab states.
        render_loading(frame, app, chunks[1]);
    } else if cold_failed {
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
    if inner.is_empty() {
        return;
    }

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
    // centered column without exceeding cramped content areas.
    let content_width = inner.width.saturating_sub(4).clamp(1, 100);
    let content_height =
        u16::try_from(wrapped_line_count(diagnostic, content_width as usize).saturating_add(4))
            .unwrap_or(u16::MAX);
    let content = Rect {
        x: inner.x + inner.width.saturating_sub(content_width) / 2,
        y: inner.y + inner.height.saturating_sub(content_height) / 2,
        width: content_width,
        height: content_height.min(inner.height),
    };
    frame.render_widget(paragraph, content);
}

/// Estimate ratatui's word-wrapped height for vertical centering. Widths use
/// terminal display cells, and explicit line breaks remain distinct rows.
fn wrapped_line_count(text: &str, width: usize) -> usize {
    debug_assert!(width > 0);
    text.split('\n')
        .map(|line| {
            let mut rows = 1;
            let mut column = 0;
            for word in line.split_whitespace() {
                let word_width = UnicodeWidthStr::width(word);
                let separator = usize::from(column > 0);
                if column + separator + word_width <= width {
                    column += separator + word_width;
                    continue;
                }

                if column > 0 {
                    rows += 1;
                }
                rows += word_width.saturating_sub(1) / width;
                column = word_width % width;
                if column == 0 && word_width > 0 {
                    column = width;
                }
            }
            rows
        })
        .sum::<usize>()
        .max(1)
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
    fn wrapped_line_count_uses_terminal_width_and_explicit_lines() {
        assert_eq!(wrapped_line_count("abcd", 2), 2);
        assert_eq!(wrapped_line_count("中中", 2), 2);
        assert_eq!(wrapped_line_count("a\n\nb", 10), 3);
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
    fn cold_start_scan_keeps_usage_tab_untouched() {
        let mut app = make_app();
        app.current_tab = Tab::Usage;
        app.set_background_loading(true);

        let screen = render_screen(&mut app, 120, 32).join("\n");

        assert!(!screen.contains("Scanning session data..."), "{screen}");
        assert!(screen.contains("subscription"), "{screen}");
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
        app.set_local_report_status("Error: injected cold failure");
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
    fn cramped_cold_failure_preserves_the_content_border() {
        let width = 18;
        let height = 18;
        let mut app = make_app();
        app.set_error(Some("a diagnostic that must wrap safely".to_string()));

        let lines = render_screen(&mut app, width, height);
        let content_rows = &lines[3..height as usize - 5];

        assert!(content_rows[0].ends_with('┐'));
        assert!(content_rows.last().unwrap().ends_with('┘'));
        assert!(content_rows[1..content_rows.len() - 1]
            .iter()
            .all(|line| line.ends_with('│')));
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
            screen.contains("Data Size"),
            "installed generation must keep the tab content visible: {screen}"
        );
        assert!(
            screen.contains("Refreshing cached data in background..."),
            "{screen}"
        );
    }
}
