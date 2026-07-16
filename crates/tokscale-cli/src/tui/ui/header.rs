use std::borrow::Cow;

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
    let block = header_block(app);
    let tabs_area = block.inner(area);
    let (visible_tabs, label_mode) = fitted_tabs(app, tabs_area);
    let titles = visible_tabs
        .iter()
        .map(|tab| {
            let style = if *tab == app.current_tab {
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.muted)
            };
            Line::from(Span::styled(tab_label(*tab, label_mode), style))
        })
        .collect::<Vec<_>>();
    let selected = visible_tabs
        .iter()
        .position(|tab| *tab == app.current_tab)
        .unwrap_or(0);

    frame.render_widget(
        Tabs::new(titles)
            .block(block)
            .select(selected)
            .highlight_style(
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .padding(TAB_PADDING_LEFT, TAB_PADDING_RIGHT)
            .divider(Span::styled(
                TAB_DIVIDER,
                Style::default().fg(app.theme.border),
            )),
        area,
    );
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
            Line::from(Span::styled(
                " local usage intelligence ",
                app.theme.subtle_text_style(),
            ))
            .right_aligned(),
        );
    }
    block
}

fn tab_label(tab: Tab, mode: TabLabelMode) -> Cow<'static, str> {
    match (tab, mode) {
        (Tab::Issues, TabLabelMode::Full) => Cow::Borrowed("Sessions"),
        (Tab::Issues, TabLabelMode::Short) => Cow::Borrowed("Ses"),
        (_, TabLabelMode::Full) => Cow::Borrowed(tab.as_str()),
        (_, TabLabelMode::Short) => Cow::Borrowed(tab.short_name()),
    }
}

fn tab_row_width(tabs: &[Tab], mode: TabLabelMode) -> u16 {
    if tabs.is_empty() {
        return 0;
    }
    let padding_width = TAB_PADDING_LEFT.width() + TAB_PADDING_RIGHT.width();
    let labels_width = tabs
        .iter()
        .map(|tab| tab_label(*tab, mode).width() + padding_width)
        .sum::<usize>();
    let dividers_width = TAB_DIVIDER.width() * tabs.len().saturating_sub(1);
    labels_width
        .saturating_add(dividers_width)
        .min(u16::MAX as usize) as u16
}

fn tab_label_mode(app: &App, tabs: &[Tab], area: Rect) -> TabLabelMode {
    if app.is_very_narrow() || tab_row_width(tabs, TabLabelMode::Full) > area.width {
        TabLabelMode::Short
    } else {
        TabLabelMode::Full
    }
}

fn fitted_tabs(app: &App, tabs_area: Rect) -> (Vec<Tab>, TabLabelMode) {
    let mut tabs = Tab::all()
        .iter()
        .copied()
        .filter(|tab| app.is_tab_visible(*tab))
        .collect::<Vec<_>>();
    let mode = tab_label_mode(app, &tabs, tabs_area);

    while tab_row_width(&tabs, mode) > tabs_area.width {
        let Some(index) = tabs.iter().enumerate().rev().find_map(|(index, tab)| {
            (*tab != Tab::Issues && *tab != app.current_tab).then_some(index)
        }) else {
            break;
        };
        tabs.remove(index);
    }
    if tab_row_width(&tabs, mode) > tabs_area.width && app.current_tab != Tab::Issues {
        tabs.retain(|tab| *tab == app.current_tab);
    }
    (tabs, mode)
}

fn renderable_tab_row(tabs_area: Rect) -> Option<Rect> {
    if tabs_area.is_empty() {
        None
    } else {
        Some(Rect::new(tabs_area.x, tabs_area.y, tabs_area.width, 1))
    }
}

fn tab_click_areas(app: &App, tabs_area: Rect) -> Vec<(Rect, Tab)> {
    let Some(tab_row) = renderable_tab_row(tabs_area) else {
        return Vec::new();
    };
    let (visible_tabs, mode) = fitted_tabs(app, tabs_area);
    let left_padding = TAB_PADDING_LEFT.width() as u16;
    let right_padding = TAB_PADDING_RIGHT.width() as u16;
    let divider_width = TAB_DIVIDER.width() as u16;
    let mut x = tab_row.x;
    let right = tab_row.right();
    let mut areas = Vec::with_capacity(visible_tabs.len());

    for (index, tab) in visible_tabs.iter().enumerate() {
        let start = x;
        x = x.saturating_add(left_padding.min(right.saturating_sub(x)));
        let label_width = (tab_label(*tab, mode).width() as u16).min(right.saturating_sub(x));
        x = x.saturating_add(label_width);
        x = x.saturating_add(right_padding.min(right.saturating_sub(x)));
        let width = x.saturating_sub(start);
        if width > 0 {
            areas.push((Rect::new(start, tab_row.y, width, tab_row.height), *tab));
        }
        if index + 1 == visible_tabs.len() || x >= right {
            break;
        }
        x = x.saturating_add(divider_width.min(right.saturating_sub(x)));
    }
    areas
}

fn register_tab_click_areas(app: &mut App, tabs_area: Rect) {
    for (rect, tab) in tab_click_areas(app, tabs_area) {
        app.add_click_area(rect, ClickAction::Tab(tab));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_slot_is_presented_as_sessions() {
        assert_eq!(tab_label(Tab::Issues, TabLabelMode::Full), "Sessions");
        assert_eq!(tab_label(Tab::Issues, TabLabelMode::Short), "Ses");
    }
}
