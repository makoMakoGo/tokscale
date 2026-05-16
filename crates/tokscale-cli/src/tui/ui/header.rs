use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Tabs};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::{App, ClickAction, Tab};

const TAB_PADDING_LEFT: &str = " ";
const TAB_PADDING_RIGHT: &str = " ";
const TAB_DIVIDER: &str = " │ ";

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let is_very_narrow = app.is_very_narrow();

    let titles: Vec<Line> = Tab::all()
        .iter()
        .map(|t| {
            let name = if is_very_narrow {
                t.short_name()
            } else {
                t.as_str()
            };
            let style = if *t == app.current_tab {
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.muted)
            };
            Line::from(Span::styled(name, style))
        })
        .collect();

    let selected = Tab::all()
        .iter()
        .position(|t| *t == app.current_tab)
        .unwrap_or(0);

    let block = header_block(app);

    let tabs = Tabs::new(titles)
        .block(block)
        .select(selected)
        .highlight_style(
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .padding(TAB_PADDING_LEFT, TAB_PADDING_RIGHT)
        .divider(tab_divider(app));

    frame.render_widget(tabs, area);

    register_tab_click_areas(app, area);
}

fn header_block(app: &App) -> Block<'static> {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " tokscale ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Left)
        .style(Style::default().bg(app.theme.background));

    if !app.is_narrow() {
        block = block.title_top(
            Line::from(vec![
                Span::styled(" | ", Style::default().fg(Color::Rgb(102, 102, 102))),
                Span::styled("GitHub ", Style::default().fg(Color::Rgb(102, 102, 102))),
            ])
            .right_aligned(),
        );
    }

    block
}

fn tab_divider(app: &App) -> Span<'static> {
    Span::styled(TAB_DIVIDER, Style::default().fg(app.theme.border))
}

fn register_tab_click_areas(app: &mut App, area: Rect) {
    let is_very_narrow = app.is_very_narrow();
    let tabs_area = header_block(app).inner(area);
    let mut x = tabs_area.x;
    let y = tabs_area.y;
    let right = tabs_area.right();

    let left_padding_width = TAB_PADDING_LEFT.width() as u16;
    let right_padding_width = TAB_PADDING_RIGHT.width() as u16;
    let divider_width = TAB_DIVIDER.width() as u16;

    for (index, tab) in Tab::all().iter().enumerate() {
        let remaining_width = right.saturating_sub(x);
        if remaining_width == 0 {
            break;
        }
        x = x.saturating_add(left_padding_width.min(remaining_width));

        let remaining_width = right.saturating_sub(x);
        if remaining_width == 0 {
            break;
        }

        let name = if is_very_narrow {
            tab.short_name()
        } else {
            tab.as_str()
        };
        let width = (name.width() as u16).min(remaining_width);
        if width > 0 {
            app.add_click_area(Rect::new(x, y, width, 1), ClickAction::Tab(*tab));
        }
        x = x.saturating_add(width);

        let remaining_width = right.saturating_sub(x);
        if remaining_width == 0 {
            break;
        }
        x = x.saturating_add(right_padding_width.min(remaining_width));

        let remaining_width = right.saturating_sub(x);
        if remaining_width == 0 || index + 1 == Tab::all().len() {
            break;
        }
        x = x.saturating_add(divider_width.min(remaining_width));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::tui::TuiConfig;

    fn make_app(width: u16) -> App {
        let config = TuiConfig {
            theme: "blue".to_string(),
            refresh: 0,
            sessions_path: None,
            clients: None,
            since: None,
            until: None,
            year: None,
            initial_tab: None,
        };
        let mut app = App::new_with_cached_data(config, None).unwrap();
        app.handle_resize(width, 24);
        app
    }

    fn registered_tab_areas(app: &App) -> Vec<(Rect, Tab)> {
        app.click_areas
            .iter()
            .filter_map(|area| match &area.action {
                ClickAction::Tab(tab) => Some((area.rect, *tab)),
                _ => None,
            })
            .collect()
    }

    fn render_header_symbols(
        app: &mut App,
        area: Rect,
        width: u16,
        height: u16,
    ) -> Vec<Vec<String>> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let frame = terminal
            .draw(|frame| {
                render(frame, app, area);
            })
            .unwrap();

        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| frame.buffer.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn symbols_at(lines: &[Vec<String>], y: u16, x: u16, width: u16) -> String {
        lines[y as usize][x as usize..(x + width) as usize].join("")
    }

    fn assert_clicks_select_tabs(app: &mut App, expected: &[(Rect, Tab)]) {
        for (rect, tab) in expected {
            for column in rect.x..rect.x + rect.width {
                app.current_tab = if *tab == Tab::Overview {
                    Tab::Agents
                } else {
                    Tab::Overview
                };

                app.handle_mouse_event(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                });

                assert_eq!(
                    app.current_tab, *tab,
                    "clicking column {column} on {tab:?} label should select {tab:?}"
                );
            }
        }
    }

    #[test]
    fn tab_click_areas_start_at_rendered_labels_for_offset_area() {
        let mut app = make_app(120);
        let area = Rect::new(20, 4, 80, 3);

        let lines = render_header_symbols(&mut app, area, 120, 8);

        assert_eq!(symbols_at(&lines, 5, 22, 8), "Overview");
        assert_eq!(symbols_at(&lines, 5, 35, 6), "Models");
        assert_eq!(symbols_at(&lines, 5, 46, 5), "Daily");
        assert_eq!(symbols_at(&lines, 5, 56, 6), "Hourly");
        assert_eq!(symbols_at(&lines, 5, 67, 5), "Stats");
        assert_eq!(symbols_at(&lines, 5, 77, 6), "Agents");
        assert_eq!(
            registered_tab_areas(&app),
            vec![
                (Rect::new(22, 5, 8, 1), Tab::Overview),
                (Rect::new(35, 5, 6, 1), Tab::Models),
                (Rect::new(46, 5, 5, 1), Tab::Daily),
                (Rect::new(56, 5, 6, 1), Tab::Hourly),
                (Rect::new(67, 5, 5, 1), Tab::Stats),
                (Rect::new(77, 5, 6, 1), Tab::Agents),
            ]
        );
    }

    #[test]
    fn tab_click_areas_use_short_labels_when_very_narrow() {
        let mut app = make_app(50);
        let area = Rect::new(7, 2, 52, 3);

        let lines = render_header_symbols(&mut app, area, 65, 6);

        assert_eq!(symbols_at(&lines, 3, 9, 3), "Ovw");
        assert_eq!(symbols_at(&lines, 3, 17, 3), "Mod");
        assert_eq!(symbols_at(&lines, 3, 25, 3), "Day");
        assert_eq!(symbols_at(&lines, 3, 33, 2), "Hr");
        assert_eq!(symbols_at(&lines, 3, 40, 3), "Sta");
        assert_eq!(symbols_at(&lines, 3, 48, 3), "Agt");
        assert_eq!(
            registered_tab_areas(&app),
            vec![
                (Rect::new(9, 3, 3, 1), Tab::Overview),
                (Rect::new(17, 3, 3, 1), Tab::Models),
                (Rect::new(25, 3, 3, 1), Tab::Daily),
                (Rect::new(33, 3, 2, 1), Tab::Hourly),
                (Rect::new(40, 3, 3, 1), Tab::Stats),
                (Rect::new(48, 3, 3, 1), Tab::Agents),
            ]
        );
    }

    #[test]
    fn clicks_on_rendered_tab_labels_select_matching_tabs() {
        let mut app = make_app(120);
        let area = Rect::new(20, 4, 80, 3);

        render_header_symbols(&mut app, area, 120, 8);

        assert_clicks_select_tabs(
            &mut app,
            &[
                (Rect::new(22, 5, 8, 1), Tab::Overview),
                (Rect::new(35, 5, 6, 1), Tab::Models),
                (Rect::new(46, 5, 5, 1), Tab::Daily),
                (Rect::new(56, 5, 6, 1), Tab::Hourly),
                (Rect::new(67, 5, 5, 1), Tab::Stats),
                (Rect::new(77, 5, 6, 1), Tab::Agents),
            ],
        );
    }

    #[test]
    fn clicks_on_very_narrow_rendered_tab_labels_select_matching_tabs() {
        let mut app = make_app(50);
        let area = Rect::new(7, 2, 52, 3);

        render_header_symbols(&mut app, area, 65, 6);

        assert_clicks_select_tabs(
            &mut app,
            &[
                (Rect::new(9, 3, 3, 1), Tab::Overview),
                (Rect::new(17, 3, 3, 1), Tab::Models),
                (Rect::new(25, 3, 3, 1), Tab::Daily),
                (Rect::new(33, 3, 2, 1), Tab::Hourly),
                (Rect::new(40, 3, 3, 1), Tab::Stats),
                (Rect::new(48, 3, 3, 1), Tab::Agents),
            ],
        );
    }
}
