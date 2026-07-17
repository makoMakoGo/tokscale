use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph};

use super::widgets::{format_cost, format_tokens};
use crate::tui::app::{App, SortField, Tab};
use crate::tui::view_state::ViewState;

pub(crate) fn render(frame: &mut Frame, app: &mut App, state: &mut ViewState, area: Rect) {
    super::footer::render(frame, app, area);
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    if inner.is_empty() {
        return;
    }

    if app.current_tab == Tab::Issues {
        render_sessions_main_row(frame, app, state, row(inner, 0));
        if inner.height >= 2 {
            render_sessions_help_row(frame, app, state, row(inner, 1));
        }
    } else if app.current_tab == Tab::Daily && !app.is_daily_detail_active() && inner.height >= 2 {
        render_daily_help_row(frame, app, state, row(inner, 1));
    }
}

fn row(area: Rect, offset: u16) -> Rect {
    Rect::new(area.x, area.y.saturating_add(offset), area.width, 1)
}

fn clear_row(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme.background)),
        area,
    );
}

fn render_sessions_main_row(frame: &mut Frame, app: &App, state: &ViewState, area: Rect) {
    clear_row(frame, app, area);
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);

    if !app.is_very_narrow() {
        let mut spans = vec![Span::styled("Sort: ", Style::default().fg(app.theme.muted))];
        for field in [SortField::Date, SortField::Tokens, SortField::Cost] {
            let active = app.sort_field == field;
            spans.push(Span::styled(
                session_sort_label(state, field),
                Style::default()
                    .fg(if active {
                        app.theme.foreground
                    } else {
                        app.theme.muted
                    })
                    .add_modifier(if active {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ));
            spans.push(Span::raw(" "));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
    }

    let count = if state.session_detail_active() {
        format!(" ({} sessions)", state.session_count())
    } else {
        format!(
            " ({} sources · {} sessions)",
            state.source_count(),
            state.session_count()
        )
    };
    let right = Line::from(vec![
        Span::styled(
            format_tokens(app.data.total_tokens),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(" tokens | ", Style::default().fg(app.theme.muted)),
        Span::styled(
            format_cost(app.data.total_cost),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(count, Style::default().fg(app.theme.muted)),
    ]);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), chunks[1]);
}

fn render_sessions_help_row(frame: &mut Frame, app: &App, state: &ViewState, area: Rect) {
    clear_row(frame, app, area);
    let text = if app.is_very_narrow() {
        if state.session_detail_active() {
            "↑↓·d/t/c·esc·s·r·←→·q".to_string()
        } else {
            "↑↓·d/t/c·↵·s·r·←→·q".to_string()
        }
    } else if state.session_detail_active() {
        "↑↓ scroll • [d:active / t:tokens / c:cost] • [esc:back] • [s:sources] • [r:refresh local] • ←→/tab view • e • q".to_string()
    } else {
        "↑↓ scroll • [d:active / t:sessions / c:space] • [enter:sessions] • [s:sources] • [r:refresh local] • ←→/tab view • e • q".to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(app.theme.muted),
        ))),
        area,
    );
}

fn render_daily_help_row(frame: &mut Frame, app: &App, state: &ViewState, area: Rect) {
    clear_row(frame, app, area);
    let view = if state.daily_profile_active() {
        "table"
    } else {
        "profile"
    };
    let text = if state.daily_profile_active() {
        if app.is_very_narrow() {
            format!("←→·v:{view}·s·g·p·r·q")
        } else {
            format!(
                "←→/tab view • [v:{view}] • [s:sources] [g:{}] • [p:{}] • [r:refresh local] • e • q",
                app.group_by.borrow(),
                app.theme.name.as_str()
            )
        }
    } else if app.is_very_narrow() {
        format!("↑↓·←→·d/t/c·↵·j·v:{view}·s·g·p·r·q")
    } else {
        format!(
            "↑↓ scroll • ←→/tab view • [d/t/c:sort] • [enter:details] [j:today] • [v:{view}] • [s:sources] [g:{}] • [p:{}] • [r:refresh local] • e • q",
            app.group_by.borrow(),
            app.theme.name.as_str()
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(app.theme.muted),
        ))),
        area,
    );
}

fn session_sort_label(state: &ViewState, field: SortField) -> &'static str {
    if state.session_detail_active() {
        match field {
            SortField::Date => "Active",
            SortField::Tokens => "Tokens",
            SortField::Cost => "Cost",
        }
    } else {
        match field {
            SortField::Date => "Active",
            SortField::Tokens => "Sessions",
            SortField::Cost => "Space",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::TuiConfig;
    use ratatui::{backend::TestBackend, Terminal};

    fn make_app(width: u16) -> App {
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
        let mut app = App::new_with_cached_data(config, None).unwrap();
        app.current_tab = Tab::Issues;
        app.terminal_width = width;
        app
    }

    #[test]
    fn sessions_footer_erases_the_removed_issues_copy() {
        let width = 140;
        let height = 5;
        let mut app = make_app(width);
        let mut state = ViewState::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| render(frame, &mut app, &mut state, frame.area()))
            .unwrap();

        let screen = terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(screen.contains("enter:sessions"));
        assert!(!screen.contains("sort coverage"));
        assert!(!screen.contains("model-session links"));
    }
}
