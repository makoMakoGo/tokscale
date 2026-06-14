use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Tabs};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::{App, ClickAction, Tab};

const TAB_PADDING_LEFT: &str = " ";
const TAB_PADDING_RIGHT: &str = " ";
const TAB_DIVIDER: &str = " │ ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabLabelMode {
    Full,
    Short,
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible_tabs: Vec<Tab> = Tab::all()
        .iter()
        .copied()
        .filter(|t| app.is_tab_visible(*t))
        .collect();
    let block = header_block(app);
    let tabs_area = block.inner(area);
    let label_mode = tab_label_mode(app, &visible_tabs, tabs_area);

    let titles: Vec<Line> = visible_tabs
        .iter()
        .map(|t| {
            let name = tab_label(*t, label_mode);
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

    let selected = visible_tabs
        .iter()
        .position(|t| *t == app.current_tab)
        .unwrap_or(0);

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

    register_tab_click_areas(app, tabs_area);
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
                Span::styled(" | ", app.theme.subtle_text_style()),
                Span::styled("GitHub ", app.theme.subtle_text_style()),
            ])
            .right_aligned(),
        );
    }

    block
}

fn tab_divider(app: &App) -> Span<'static> {
    Span::styled(TAB_DIVIDER, Style::default().fg(app.theme.border))
}

fn tab_label(tab: Tab, mode: TabLabelMode) -> &'static str {
    match mode {
        TabLabelMode::Full => tab.as_str(),
        TabLabelMode::Short => tab.short_name(),
    }
}

fn tab_row_width(tabs: &[Tab], mode: TabLabelMode) -> u16 {
    if tabs.is_empty() {
        return 0;
    }

    let padding_width = TAB_PADDING_LEFT.width() + TAB_PADDING_RIGHT.width();
    let labels_width: usize = tabs
        .iter()
        .map(|tab| tab_label(*tab, mode).width() + padding_width)
        .sum();
    let dividers_width = TAB_DIVIDER.width() * tabs.len().saturating_sub(1);
    labels_width
        .saturating_add(dividers_width)
        .min(u16::MAX as usize) as u16
}

fn tab_label_mode(app: &App, tabs: &[Tab], tabs_area: Rect) -> TabLabelMode {
    if app.is_very_narrow() || tab_row_width(tabs, TabLabelMode::Full) > tabs_area.width {
        TabLabelMode::Short
    } else {
        TabLabelMode::Full
    }
}

fn tab_click_areas(app: &App, tabs_area: Rect) -> Vec<(Rect, Tab)> {
    let Some(tab_row) = renderable_tab_row(tabs_area) else {
        return Vec::new();
    };

    let visible_tabs: Vec<Tab> = Tab::all()
        .iter()
        .copied()
        .filter(|t| app.is_tab_visible(*t))
        .collect();
    let label_mode = tab_label_mode(app, &visible_tabs, tabs_area);
    let mut areas = Vec::with_capacity(visible_tabs.len());
    let mut x = tab_row.x;
    let right = tab_row.right();

    let left_padding_width = TAB_PADDING_LEFT.width() as u16;
    let right_padding_width = TAB_PADDING_RIGHT.width() as u16;
    let divider_width = TAB_DIVIDER.width() as u16;

    for (index, tab) in visible_tabs.iter().enumerate() {
        let tab_start = x;
        let remaining_width = right.saturating_sub(x);
        if remaining_width == 0 {
            break;
        }
        x = x.saturating_add(left_padding_width.min(remaining_width));

        let remaining_width = right.saturating_sub(x);
        if remaining_width == 0 {
            break;
        }

        let name = tab_label(*tab, label_mode);
        let width = (name.width() as u16).min(remaining_width);
        if width == 0 {
            break;
        }
        x = x.saturating_add(width);

        let remaining_width = right.saturating_sub(x);
        x = x.saturating_add(right_padding_width.min(remaining_width));

        let tab_width = x.saturating_sub(tab_start);
        if tab_width > 0 {
            areas.push((
                Rect::new(tab_start, tab_row.y, tab_width, tab_row.height),
                *tab,
            ));
        }

        let remaining_width = right.saturating_sub(x);
        if remaining_width == 0 || index + 1 == visible_tabs.len() {
            break;
        }
        x = x.saturating_add(divider_width.min(remaining_width));
    }

    areas
}

fn renderable_tab_row(tabs_area: Rect) -> Option<Rect> {
    // Ratatui's Tabs render tab content on the first row of the block inner area.
    // If that inner area is empty, no tab content is renderable and therefore no
    // click hitboxes should exist.
    if tabs_area.is_empty() {
        return None;
    }

    Some(Rect::new(tabs_area.x, tabs_area.y, tabs_area.width, 1))
}

fn register_tab_click_areas(app: &mut App, tabs_area: Rect) {
    for (rect, tab) in tab_click_areas(app, tabs_area) {
        app.add_click_area(rect, ClickAction::Tab(tab));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{backend::TestBackend, Terminal};

    use crate::tui::app::TuiConfig;

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
        app.settings.usage_tab_enabled = false;
        app.handle_resize(width, 24);
        app
    }

    fn make_app_with_usage(width: u16) -> App {
        let mut app = make_app(width);
        app.settings.usage_tab_enabled = true;
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

    fn expected_normal_tab_areas() -> Vec<(Rect, Tab)> {
        vec![
            (Rect::new(21, 5, 10, 1), Tab::Overview),
            (Rect::new(34, 5, 8, 1), Tab::Models),
            (Rect::new(45, 5, 9, 1), Tab::Monthly),
            (Rect::new(57, 5, 8, 1), Tab::Weekly),
            (Rect::new(68, 5, 7, 1), Tab::Daily),
            (Rect::new(78, 5, 8, 1), Tab::Hourly),
            (Rect::new(89, 5, 7, 1), Tab::Stats),
            (Rect::new(99, 5, 8, 1), Tab::Agents),
        ]
    }

    fn expected_normal_tab_areas_with_usage() -> Vec<(Rect, Tab)> {
        vec![
            (Rect::new(21, 5, 10, 1), Tab::Overview),
            (Rect::new(34, 5, 7, 1), Tab::Usage),
            (Rect::new(44, 5, 8, 1), Tab::Models),
            (Rect::new(55, 5, 9, 1), Tab::Monthly),
            (Rect::new(67, 5, 8, 1), Tab::Weekly),
            (Rect::new(78, 5, 7, 1), Tab::Daily),
            (Rect::new(88, 5, 8, 1), Tab::Hourly),
            (Rect::new(99, 5, 7, 1), Tab::Stats),
            (Rect::new(109, 5, 8, 1), Tab::Agents),
        ]
    }

    fn expected_very_narrow_tab_areas() -> Vec<(Rect, Tab)> {
        vec![
            (Rect::new(8, 3, 5, 1), Tab::Overview),
            (Rect::new(16, 3, 5, 1), Tab::Models),
            (Rect::new(24, 3, 5, 1), Tab::Monthly),
            (Rect::new(32, 3, 4, 1), Tab::Weekly),
            (Rect::new(39, 3, 5, 1), Tab::Daily),
            (Rect::new(47, 3, 4, 1), Tab::Hourly),
            (Rect::new(54, 3, 5, 1), Tab::Stats),
            (Rect::new(62, 3, 5, 1), Tab::Agents),
        ]
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
                    "clicking column {column} on {tab:?} hitbox should select {tab:?}"
                );
            }
        }
    }

    fn assert_clicks_do_not_switch_tabs(app: &mut App, dividers: &[Rect]) {
        for rect in dividers {
            for column in rect.x..rect.x + rect.width {
                app.current_tab = Tab::Agents;

                app.handle_mouse_event(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                });

                assert_eq!(
                    app.current_tab,
                    Tab::Agents,
                    "clicking divider column {column} should not switch tabs"
                );
            }
        }
    }

    #[test]
    fn tab_click_areas_are_empty_without_renderable_tab_row() {
        let app = make_app(120);

        for area in [
            Rect::new(21, 5, 78, 0),
            Rect::new(21, 5, 0, 1),
            Rect::new(21, 5, 0, 0),
        ] {
            assert!(
                tab_click_areas(&app, area).is_empty(),
                "non-renderable tabs area {area} should not produce click hitboxes"
            );
        }
    }

    #[test]
    fn tab_click_areas_match_normal_renderable_tab_segments() {
        let app = make_app(120);

        assert_eq!(
            tab_click_areas(&app, Rect::new(21, 5, 90, 1)),
            expected_normal_tab_areas()
        );
    }

    #[test]
    fn tab_click_areas_include_usage_when_enabled() {
        let app = make_app_with_usage(120);

        assert_eq!(
            tab_click_areas(&app, Rect::new(21, 5, 100, 1)),
            expected_normal_tab_areas_with_usage()
        );
    }

    #[test]
    fn tab_click_areas_match_very_narrow_renderable_tab_segments() {
        let app = make_app(50);

        assert_eq!(
            tab_click_areas(&app, Rect::new(8, 3, 65, 1)),
            expected_very_narrow_tab_areas()
        );
    }

    #[test]
    fn rendered_normal_tabs_match_click_area_geometry_for_offset_area() {
        let mut app = make_app(120);
        let area = Rect::new(20, 4, 92, 3);

        let lines = render_header_symbols(&mut app, area, 120, 8);

        assert_eq!(symbols_at(&lines, 5, 21, 10), " Overview ");
        assert_eq!(symbols_at(&lines, 5, 34, 8), " Models ");
        assert_eq!(symbols_at(&lines, 5, 45, 9), " Monthly ");
        assert_eq!(symbols_at(&lines, 5, 57, 8), " Weekly ");
        assert_eq!(symbols_at(&lines, 5, 68, 7), " Daily ");
        assert_eq!(symbols_at(&lines, 5, 78, 8), " Hourly ");
        assert_eq!(symbols_at(&lines, 5, 89, 7), " Stats ");
        assert_eq!(symbols_at(&lines, 5, 99, 8), " Agents ");
        assert_eq!(registered_tab_areas(&app), expected_normal_tab_areas());
    }

    #[test]
    fn rendered_very_narrow_tabs_match_click_area_geometry() {
        let mut app = make_app(50);
        let area = Rect::new(7, 2, 65, 3);

        let lines = render_header_symbols(&mut app, area, 80, 6);

        assert_eq!(symbols_at(&lines, 3, 8, 5), " Ovw ");
        assert_eq!(symbols_at(&lines, 3, 16, 5), " Mod ");
        assert_eq!(symbols_at(&lines, 3, 24, 5), " Mth ");
        assert_eq!(symbols_at(&lines, 3, 32, 4), " Wk ");
        assert_eq!(symbols_at(&lines, 3, 39, 5), " Day ");
        assert_eq!(symbols_at(&lines, 3, 47, 4), " Hr ");
        assert_eq!(symbols_at(&lines, 3, 54, 5), " Sta ");
        assert_eq!(symbols_at(&lines, 3, 62, 5), " Agt ");
        assert_eq!(registered_tab_areas(&app), expected_very_narrow_tab_areas());
    }

    #[test]
    fn clicks_on_tab_dividers_do_not_switch_tabs() {
        let mut app = make_app(120);
        let area = Rect::new(20, 4, 92, 3);

        let lines = render_header_symbols(&mut app, area, 120, 8);

        assert_eq!(symbols_at(&lines, 5, 31, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 5, 42, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 5, 54, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 5, 65, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 5, 75, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 5, 86, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 5, 96, 3), TAB_DIVIDER);

        assert_clicks_do_not_switch_tabs(
            &mut app,
            &[
                Rect::new(31, 5, 3, 1),
                Rect::new(42, 5, 3, 1),
                Rect::new(54, 5, 3, 1),
                Rect::new(65, 5, 3, 1),
                Rect::new(75, 5, 3, 1),
                Rect::new(86, 5, 3, 1),
                Rect::new(96, 5, 3, 1),
            ],
        );
    }

    #[test]
    fn clicks_on_very_narrow_tab_dividers_do_not_switch_tabs() {
        let mut app = make_app(50);
        let area = Rect::new(7, 2, 65, 3);

        let lines = render_header_symbols(&mut app, area, 80, 6);

        assert_eq!(symbols_at(&lines, 3, 13, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 3, 21, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 3, 29, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 3, 36, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 3, 44, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 3, 51, 3), TAB_DIVIDER);
        assert_eq!(symbols_at(&lines, 3, 59, 3), TAB_DIVIDER);

        assert_clicks_do_not_switch_tabs(
            &mut app,
            &[
                Rect::new(13, 3, 3, 1),
                Rect::new(21, 3, 3, 1),
                Rect::new(29, 3, 3, 1),
                Rect::new(36, 3, 3, 1),
                Rect::new(44, 3, 3, 1),
                Rect::new(51, 3, 3, 1),
                Rect::new(59, 3, 3, 1),
            ],
        );
    }

    #[test]
    fn clicks_on_rendered_tab_labels_and_padding_select_matching_tabs() {
        let mut app = make_app(120);
        let area = Rect::new(20, 4, 92, 3);

        render_header_symbols(&mut app, area, 120, 8);

        assert_clicks_select_tabs(&mut app, &expected_normal_tab_areas());
    }

    #[test]
    fn clicks_on_very_narrow_rendered_tab_labels_and_padding_select_matching_tabs() {
        let mut app = make_app(50);
        let area = Rect::new(7, 2, 65, 3);

        render_header_symbols(&mut app, area, 80, 6);

        assert_clicks_select_tabs(&mut app, &expected_very_narrow_tab_areas());
    }
}
