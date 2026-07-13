use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation};
use tokscale_core::source_health::HealthReport;

use crate::tui::app::App;
use crate::tui::themes::Theme;
use crate::tui::ui::widgets::viewport_scrollbar_state;

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            " Data Issues ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = build_issue_lines(&app.theme, &app.data.health);
    let total_lines = lines.len();
    let visible_height = inner.height as usize;
    app.set_issues_text_viewport(visible_height, total_lines);

    let paragraph = Paragraph::new(
        lines
            .drain(app.issues_text_visible_range())
            .collect::<Vec<_>>(),
    );
    frame.render_widget(paragraph, inner);

    if total_lines > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));
        let mut state =
            viewport_scrollbar_state(total_lines, app.issues_viewport.scroll, visible_height);
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut state,
        );
    }
}

pub(crate) fn build_issue_lines(theme: &Theme, health: &HealthReport) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            "Summary",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                format!("Healthy sources: {}", health.healthy_sources),
                Style::default().fg(theme.foreground),
            ),
            Span::styled("  |  ", Style::default().fg(theme.muted)),
            Span::styled(
                format!("Partial sources: {}", health.partial_sources),
                Style::default().fg(Color::Red),
            ),
            Span::styled("  |  ", Style::default().fg(theme.muted)),
            Span::styled(
                format!("Failed sources: {}", health.failed_sources),
                Style::default().fg(Color::Red),
            ),
        ]),
        Line::from(Span::styled(
            format!("Rejected records: {}", health.rejected_records),
            Style::default().fg(Color::Yellow),
        )),
    ];

    if health.issue_count() == 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "No data issues found.",
            Style::default().fg(theme.muted),
        )));
        return lines;
    }

    if health
        .sources
        .iter()
        .any(|source| !source.rejections.is_empty())
    {
        lines.push(Line::from(""));
        lines.push(section_heading("Rejected records", Color::Yellow));
        for source in &health.sources {
            for rejection in source.rejections.entries() {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{}  ", source.client),
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{}: {}", rejection.label, rejection.count),
                        Style::default().fg(Color::Yellow),
                    ),
                ]));
                lines.push(detail_line(theme, "source", &source.path));
                if let Some(sample) = rejection.sample {
                    lines.push(detail_line(theme, "sample", sample));
                }
            }
        }
    }

    if health
        .sources
        .iter()
        .any(|source| matches!(source.status.as_str(), "partial" | "unavailable"))
    {
        lines.push(Line::from(""));
        lines.push(section_heading("Source failures", Color::Red));
        for source in &health.sources {
            if !matches!(source.status.as_str(), "partial" | "unavailable") {
                continue;
            }

            let status = if source.status == "partial" {
                "Partial"
            } else {
                "Unavailable"
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}  ", source.client),
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(status, Style::default().fg(Color::Red)),
            ]));
            lines.push(detail_line(theme, "source", &source.path));
            if let Some(failure) = &source.failure {
                lines.push(detail_line(theme, &failure.operation, &failure.message));
            }
            if source.status == "partial" {
                lines.push(detail_line(theme, "affected records", "unknown"));
            }
        }
    }

    lines
}

fn section_heading(label: &'static str, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        label,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn detail_line(theme: &Theme, label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label}: "), Style::default().fg(theme.muted)),
        Span::styled(value.to_string(), Style::default().fg(theme.foreground)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokscale_core::source_health::SourceHealthReport;
    use tokscale_core::{RejectionSummary, SourceFailure};

    use crate::tui::themes::ThemeName;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn issue_lines_show_record_rejections_and_source_failures() {
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
        let mut rejections = RejectionSummary::default();
        rejections.record_key("missing-model", || "thread bad-1".to_string());
        let health = HealthReport {
            complete: false,
            healthy_sources: 7,
            rejected_records: 1,
            partial_sources: 1,
            failed_sources: 1,
            sources: vec![
                SourceHealthReport {
                    client: "zed".to_string(),
                    path: "C:/Users/test/Zed/threads/threads.db".to_string(),
                    status: "complete".to_string(),
                    failure: None,
                    rejections,
                },
                SourceHealthReport {
                    client: "opencode".to_string(),
                    path: "/tmp/opencode.db".to_string(),
                    status: "unavailable".to_string(),
                    failure: Some(SourceFailure::new("open SQLite", "database is corrupt")),
                    rejections: RejectionSummary::default(),
                },
                SourceHealthReport {
                    client: "claude".to_string(),
                    path: "/tmp/session.jsonl".to_string(),
                    status: "partial".to_string(),
                    failure: Some(SourceFailure::new("read line", "unexpected EOF")),
                    rejections: RejectionSummary::default(),
                },
            ],
        };

        let text = build_issue_lines(&theme, &health)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("Healthy sources: 7"));
        assert!(text.contains("Rejected records: 1"));
        assert!(text.contains("zed  Missing model: 1"));
        assert!(text.contains("thread bad-1"));
        assert!(text.contains("opencode  Unavailable"));
        assert!(text.contains("database is corrupt"));
        assert!(text.contains("claude  Partial"));
        assert!(text.contains("affected records: unknown"));
    }

    #[test]
    fn issue_lines_make_the_healthy_state_explicit() {
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
        let text = build_issue_lines(&theme, &HealthReport::default())
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("Rejected records: 0"));
        assert!(text.contains("No data issues found."));
    }
}
