use chrono::Datelike;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::app::App;

pub(crate) const PANEL_HEIGHT: u16 = 10;
pub(crate) const MIN_COMBINED_HEIGHT: u16 = 18;

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let mut totals = [0u64; 7];
    for day in &app.data.daily {
        let index = day.date.weekday().num_days_from_monday() as usize;
        totals[index] = totals[index]
            .checked_add(day.tokens.total())
            .expect("daily profile token total exceeds u64::MAX");
    }

    let best_index = totals
        .iter()
        .enumerate()
        .max_by(|(left_index, left), (right_index, right)| {
            left.cmp(right).then_with(|| right_index.cmp(left_index))
        })
        .map(|(index, _)| index)
        .unwrap_or(0);
    let total_tokens = totals
        .iter()
        .copied()
        .try_fold(0u64, u64::checked_add)
        .expect("daily profile total exceeds u64::MAX");
    let max_tokens = totals.iter().copied().max().unwrap_or(0);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Daily Profile ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));
    if total_tokens > 0 {
        block = block.title_top(
            Line::from(Span::styled(
                format!(" Most productive: {} ", WEEKDAYS[best_index]),
                Style::default().fg(Color::Yellow),
            ))
            .right_aligned(),
        );
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    if total_tokens == 0 {
        frame.render_widget(
            Paragraph::new("No daily usage data available")
                .style(Style::default().fg(app.theme.muted))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let bar_width = (inner.width as usize).saturating_sub(22).clamp(1, 28);
    let lines = WEEKDAYS
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let value = totals[index];
            let percentage = value as f64 / total_tokens as f64 * 100.0;
            let filled = if max_tokens > 0 {
                (value as f64 / max_tokens as f64 * bar_width as f64).round() as usize
            } else {
                0
            }
            .min(bar_width);
            let label_style = if index == best_index {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.foreground)
            };
            Line::from(vec![
                Span::styled(format!(" {label:<3} "), label_style),
                Span::styled("█".repeat(filled), Style::default().fg(Color::Green)),
                Span::styled(
                    "░".repeat(bar_width.saturating_sub(filled)),
                    app.theme.subtle_text_style(),
                ),
                Span::styled(
                    format!("  {:>5.1}%", percentage),
                    Style::default().fg(app.theme.muted),
                ),
            ])
        })
        .take(inner.height as usize)
        .collect::<Vec<_>>();

    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_and_table_have_a_deliberate_minimum_height() {
        assert!(MIN_COMBINED_HEIGHT > PANEL_HEIGHT);
    }
}
