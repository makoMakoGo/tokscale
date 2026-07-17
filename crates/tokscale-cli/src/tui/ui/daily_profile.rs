use chrono::Datelike;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::widgets::format_tokens;
use crate::tui::app::App;

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProfileLayout {
    show_values: bool,
    bar_width: usize,
}

fn profile_layout(width: u16) -> ProfileLayout {
    let show_values = width >= 30;
    let fixed_width = if show_values { 21 } else { 12 };
    ProfileLayout {
        show_values,
        bar_width: (width as usize).saturating_sub(fixed_width).min(28),
    }
}

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

    let profile_layout = profile_layout(inner.width);
    let lines = WEEKDAYS
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let value = totals[index];
            let percentage = value as f64 / total_tokens as f64 * 100.0;
            let filled = if max_tokens > 0 {
                (value as f64 / max_tokens as f64 * profile_layout.bar_width as f64).round()
                    as usize
            } else {
                0
            }
            .min(profile_layout.bar_width);
            let label_style = if index == best_index {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.foreground)
            };
            let mut spans = vec![Span::styled(format!(" {label:<3} "), label_style)];
            if profile_layout.show_values {
                spans.push(Span::styled(
                    format!("{:>8} ", format_tokens(value)),
                    Style::default().fg(app.theme.foreground),
                ));
            }
            spans.extend([
                Span::styled("█".repeat(filled), Style::default().fg(Color::Green)),
                Span::styled(
                    "░".repeat(profile_layout.bar_width.saturating_sub(filled)),
                    app.theme.subtle_text_style(),
                ),
                Span::styled(
                    format!(" {:>5.1}%", percentage),
                    Style::default().fg(app.theme.muted),
                ),
            ]);
            Line::from(spans)
        })
        .take(inner.height as usize)
        .collect::<Vec<_>>();

    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::{profile_layout, WEEKDAYS};

    #[test]
    fn profile_covers_every_weekday() {
        assert_eq!(WEEKDAYS.len(), 7);
    }

    #[test]
    fn wide_profile_reserves_aligned_values_and_caps_the_bar() {
        let layout = profile_layout(80);

        assert!(layout.show_values);
        assert_eq!(layout.bar_width, 28);
        assert!(21 + layout.bar_width <= 80);
    }

    #[test]
    fn compact_profile_drops_values_before_squeezing_labels_or_percentages() {
        let layout = profile_layout(29);

        assert!(!layout.show_values);
        assert_eq!(layout.bar_width, 17);
        assert_eq!(12 + layout.bar_width, 29);
    }
}
