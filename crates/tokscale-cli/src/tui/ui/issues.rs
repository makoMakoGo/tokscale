use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, Table,
};
use tokscale_core::source_health::HealthReport;
use unicode_width::UnicodeWidthStr;

use crate::tui::app::App;
use crate::tui::themes::Theme;
use crate::tui::ui::table_layout::{distributed_table_area, DISTRIBUTED_TABLE_FLEX};
use crate::tui::ui::widgets::{
    format_tokens_with_commas, get_client_display_name, truncate_display_width,
    viewport_scrollbar_state,
};

const LEVEL_WIDTH: usize = 5;
const SOURCE_MIN_WIDTH: usize = 6;
const SOURCE_MAX_WIDTH: usize = 20;
const ISSUE_MIN_WIDTH: usize = 8;
const ISSUE_MAX_WIDTH: usize = 32;
const SOURCES_MIN_WIDTH: usize = 7;
const SOURCES_MAX_WIDTH: usize = 10;
const RECORDS_MIN_WIDTH: usize = 7;
const RECORDS_MAX_WIDTH: usize = 10;
const HANDLING_MIN_WIDTH: usize = 8;
const HANDLING_MAX_WIDTH: usize = 20;
const DETAILS_TABLE_PREFERRED_WIDTH: u16 = 110;
const TABLE_COLUMN_SPACING: usize = 2;
const SUMMARY_DESCRIPTIONS_MIN_WIDTH: usize = 96;
const SUMMARY_GROUP_WIDTH: usize = 9;
const SUMMARY_STATUS_WIDTH: usize = 10;
const SUMMARY_VALUE_WIDTH: usize = 10;

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let summary_width = area.width.saturating_sub(4) as usize;
    let summary = summary_lines(&app.theme, &app.data.health, summary_width);
    let rows = issue_rows(&app.data.health);
    let [summary_area, details_area] = issue_panel_areas(area, summary.len(), rows.len());

    render_summary(frame, app, summary_area, summary);
    render_details(frame, app, details_area, rows);
}

fn issue_panel_areas(area: Rect, summary_lines: usize, issue_rows: usize) -> [Rect; 2] {
    let summary_height = summary_lines.saturating_add(2).min(usize::from(u16::MAX)) as u16;
    let summary_height = summary_height.min(area.height);
    let summary_area = Rect::new(area.x, area.y, area.width, summary_height);

    let remaining_height = area.height.saturating_sub(summary_height);
    let details_content_height = if issue_rows == 0 {
        1
    } else {
        issue_rows.saturating_add(1)
    };
    let details_height = details_content_height
        .saturating_add(2)
        .min(usize::from(remaining_height)) as u16;
    let details_area = Rect::new(
        area.x,
        area.y.saturating_add(summary_height),
        area.width,
        details_height,
    );

    [summary_area, details_area]
}

fn panel_block<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(app.theme.background))
}

fn render_summary(frame: &mut Frame, app: &App, area: Rect, lines: Vec<Line<'static>>) {
    let block = panel_block(app, "Summary");
    let inner = distributed_table_area(block.inner(area));
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_details(frame: &mut Frame, app: &mut App, area: Rect, rows: Vec<IssueRow>) {
    let block = panel_block(app, "Details");
    let inner = distributed_table_area(block.inner(area));
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if rows.is_empty() {
        app.set_issues_text_viewport(inner.height as usize, 0);
        frame.render_widget(
            Paragraph::new("No data issues found.").style(Style::default().fg(app.theme.muted)),
            inner,
        );
        return;
    }

    let visible_height = inner.height.saturating_sub(1) as usize;
    app.set_issues_text_viewport(visible_height, rows.len());
    let layout = IssueTableLayout::for_rows(inner.width as usize, &rows);
    let visible_rows = rows[app.issues_text_visible_range()]
        .iter()
        .map(|row| record_table_row(&app.theme, row, layout))
        .collect::<Vec<_>>();
    let header = Row::new(vec![
        Cell::from("LEVEL"),
        Cell::from("SOURCE"),
        Cell::from(Line::from("ISSUE").centered()),
        Cell::from(Line::from("SOURCES").centered()),
        Cell::from(Line::from("RECORDS").centered()),
        Cell::from("HANDLING"),
    ])
    .style(
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let table = Table::new(visible_rows, layout.constraints())
        .header(header)
        .column_spacing(TABLE_COLUMN_SPACING as u16)
        .flex(DISTRIBUTED_TABLE_FLEX);
    frame.render_widget(table, details_table_area(inner, layout));

    if rows.len() > visible_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));
        let mut state =
            viewport_scrollbar_state(rows.len(), app.issues_viewport.scroll, visible_height);
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

fn issue_rows(health: &HealthReport) -> Vec<IssueRow> {
    health
        .issues
        .iter()
        .map(|issue| {
            let (level, color) = match issue.level.as_str() {
                "warning" => ("WARN", Color::Yellow),
                "error" => ("ERROR", Color::Red),
                other => panic!("unsupported health issue level `{other}`"),
            };
            IssueRow {
                level,
                color,
                source: get_client_display_name(&issue.source),
                issue: issue_label(&issue.issue).to_string(),
                affected_sources: issue.affected_sources,
                rejected_records: issue.rejected_records,
                handling: handling_label(&issue.handling),
            }
        })
        .collect()
}

fn issue_label(key: &str) -> &str {
    match key {
        "missing-model" => "Missing Model",
        "missing-provider" => "Missing Provider",
        "missing-timestamp" => "Missing Timestamp",
        "malformed-record" => "Malformed Record",
        "partial-source" => "Partial Source",
        "source-unavailable" => "Source Unavailable",
        other => other,
    }
}

fn handling_label(key: &str) -> &'static str {
    match key {
        "record-skipped" => "Record Skipped",
        "confirmed-data-kept" => "Confirmed Data Kept",
        "source-skipped" => "Source Skipped",
        other => panic!("unsupported health issue handling `{other}`"),
    }
}

fn details_table_area(area: Rect, layout: IssueTableLayout) -> Rect {
    Rect {
        width: area
            .width
            .min((layout.rendered_width() as u16).max(DETAILS_TABLE_PREFERRED_WIDTH)),
        ..area
    }
}

fn record_table_row(theme: &Theme, row: &IssueRow, layout: IssueTableLayout) -> Row<'static> {
    Row::new(vec![
        Cell::from(row.level).style(Style::default().fg(row.color).add_modifier(Modifier::BOLD)),
        Cell::from(truncate_display_width(&row.source, layout.source))
            .style(Style::default().fg(theme.muted)),
        Cell::from(Line::from(truncate_display_width(&row.issue, layout.issue)).centered())
            .style(Style::default().fg(theme.foreground)),
        Cell::from(Line::from(row.affected_sources.to_string()).centered())
            .style(Style::default().fg(row.color)),
        Cell::from(
            Line::from(
                row.rejected_records
                    .map_or_else(|| "—".to_string(), |count| count.to_string()),
            )
            .centered(),
        )
        .style(Style::default().fg(if row.rejected_records.is_some() {
            row.color
        } else {
            theme.muted
        })),
        Cell::from(truncate_display_width(row.handling, layout.handling))
            .style(Style::default().fg(theme.foreground)),
    ])
}

struct SummaryStatus {
    group: &'static str,
    label: &'static str,
    value: String,
    color: Color,
    explanation: &'static str,
}

fn summary_lines(
    theme: &Theme,
    health: &HealthReport,
    available_width: usize,
) -> Vec<Line<'static>> {
    let show_explanations = available_width >= SUMMARY_DESCRIPTIONS_MIN_WIDTH;
    let source_statuses = [
        SummaryStatus {
            group: "Sources",
            label: "Clean",
            value: format_tokens_with_commas(health.clean_sources as u64),
            color: if health.clean_sources > 0 {
                theme.accent
            } else {
                theme.muted
            },
            explanation: "Scan completed; no records rejected",
        },
        SummaryStatus {
            group: "",
            label: "Degraded",
            value: format_tokens_with_commas(health.degraded_sources as u64),
            color: if health.degraded_sources > 0 {
                Color::Yellow
            } else {
                theme.muted
            },
            explanation: "Scan completed; invalid records skipped",
        },
        SummaryStatus {
            group: "",
            label: "Partial",
            value: format_tokens_with_commas(health.partial_sources as u64),
            color: if health.partial_sources > 0 {
                Color::Red
            } else {
                theme.muted
            },
            explanation: "Scan interrupted; confirmed data kept",
        },
        SummaryStatus {
            group: "",
            label: "Failed",
            value: format_tokens_with_commas(health.failed_sources as u64),
            color: if health.failed_sources > 0 {
                Color::Red
            } else {
                theme.muted
            },
            explanation: "Source unavailable; source skipped",
        },
    ];
    let record_status = SummaryStatus {
        group: "Records",
        label: "Rejected",
        value: format_tokens_with_commas(health.rejected_records),
        color: if health.rejected_records > 0 {
            Color::Yellow
        } else {
            theme.muted
        },
        explanation: "Invalid records skipped",
    };

    let mut lines = vec![
        source_health_line(theme, health, show_explanations),
        source_data_line(theme, health.source_data_bytes, show_explanations),
        Line::default(),
    ];
    lines.extend(
        source_statuses
            .iter()
            .map(|status| summary_status_line(theme, status, show_explanations)),
    );
    lines.push(summary_status_line(
        theme,
        &record_status,
        show_explanations,
    ));
    lines
}

fn source_health_line(
    theme: &Theme,
    health: &HealthReport,
    show_explanation: bool,
) -> Line<'static> {
    let total_sources = total_sources(health);
    let percentage = source_health_percentage(health);
    let health_color = source_health_color(theme, health, total_sources);
    let mut spans = vec![Span::styled(
        format!(
            "{:<width$}",
            "Source Health",
            width = SUMMARY_GROUP_WIDTH + 5
        ),
        Style::default()
            .fg(theme.foreground)
            .add_modifier(Modifier::BOLD),
    )];
    if total_sources == 0 {
        spans.push(Span::styled(
            format!("{:>width$}", "—", width = SUMMARY_VALUE_WIDTH),
            Style::default().fg(theme.muted),
        ));
    } else {
        spans.push(Span::styled(
            format!(" {:>8} ", percentage),
            Style::default()
                .fg(theme.background)
                .bg(health_color)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if show_explanation {
        let explanation = if total_sources == 0 {
            "No source units discovered at the latest refresh.".to_string()
        } else {
            format!(
                "{} of {} source units are clean at the latest refresh.",
                format_tokens_with_commas(health.clean_sources as u64),
                format_tokens_with_commas(total_sources as u64),
            )
        };
        spans.push(Span::styled("   ", Style::default()));
        spans.push(Span::styled(explanation, Style::default().fg(theme.muted)));
    }
    Line::from(spans)
}

fn source_data_line(
    theme: &Theme,
    source_data_bytes: u64,
    show_explanation: bool,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{:<width$}", "Source Data", width = SUMMARY_GROUP_WIDTH + 5),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{:>width$}",
                format_source_data_bytes(source_data_bytes),
                width = SUMMARY_VALUE_WIDTH
            ),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if show_explanation {
        spans.push(Span::styled("   ", Style::default()));
        spans.push(Span::styled(
            "Current on-disk footprint; tokscale cache excluded.",
            Style::default().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

fn summary_status_line(
    theme: &Theme,
    status: &SummaryStatus,
    show_explanation: bool,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{:<width$}", status.group, width = SUMMARY_GROUP_WIDTH),
            Style::default()
                .fg(if status.group.is_empty() {
                    theme.muted
                } else {
                    theme.foreground
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("■ ", Style::default().fg(status.color)),
        Span::styled(
            format!("{:<width$}", status.label, width = SUMMARY_STATUS_WIDTH),
            Style::default().fg(status.color),
        ),
        Span::styled(
            format!("{:>width$}", status.value, width = SUMMARY_VALUE_WIDTH),
            Style::default()
                .fg(status.color)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if show_explanation {
        spans.push(Span::styled("   ", Style::default()));
        spans.push(Span::styled(
            status.explanation,
            Style::default().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

fn total_sources(health: &HealthReport) -> usize {
    health
        .clean_sources
        .checked_add(health.degraded_sources)
        .and_then(|total| total.checked_add(health.partial_sources))
        .and_then(|total| total.checked_add(health.failed_sources))
        .expect("source count must fit in usize")
}

fn source_health_percentage(health: &HealthReport) -> String {
    let total = total_sources(health);
    if total == 0 {
        return "—".to_string();
    }
    if health.clean_sources == total {
        return "100%".to_string();
    }
    format!("{:.2}%", health.clean_sources as f64 / total as f64 * 100.0)
}

fn source_health_color(theme: &Theme, health: &HealthReport, total_sources: usize) -> Color {
    if total_sources == 0 {
        theme.muted
    } else if health.clean_sources as f64 / total_sources as f64 >= 0.99 {
        Color::Green
    } else if health.clean_sources as f64 / total_sources as f64 >= 0.95 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn format_source_data_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;

    let (unit, divisor) = if bytes >= TIB {
        ("TiB", TIB)
    } else if bytes >= GIB {
        ("GiB", GIB)
    } else if bytes >= MIB {
        ("MiB", MIB)
    } else if bytes >= KIB {
        ("KiB", KIB)
    } else {
        return format!("{} B", format_tokens_with_commas(bytes));
    };
    format!("{:.1} {unit}", bytes as f64 / divisor as f64)
}

#[derive(Debug, PartialEq, Eq)]
struct IssueRow {
    level: &'static str,
    color: Color,
    source: String,
    issue: String,
    affected_sources: u64,
    rejected_records: Option<u64>,
    handling: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IssueTableLayout {
    level: usize,
    source: usize,
    issue: usize,
    sources: usize,
    records: usize,
    handling: usize,
}

impl IssueTableLayout {
    fn for_rows(width: usize, rows: &[IssueRow]) -> Self {
        let sources = rows
            .iter()
            .map(|row| row.affected_sources.to_string().width())
            .chain(std::iter::once("SOURCES".width()))
            .max()
            .unwrap_or(SOURCES_MIN_WIDTH)
            .clamp(SOURCES_MIN_WIDTH, SOURCES_MAX_WIDTH);
        let records = rows
            .iter()
            .filter_map(|row| row.rejected_records)
            .map(|count| count.to_string().width())
            .chain(std::iter::once("RECORDS".width()))
            .max()
            .unwrap_or(RECORDS_MIN_WIDTH)
            .clamp(RECORDS_MIN_WIDTH, RECORDS_MAX_WIDTH);
        let source = rows
            .iter()
            .map(|row| row.source.width())
            .chain(std::iter::once("SOURCE".width()))
            .max()
            .unwrap_or(SOURCE_MIN_WIDTH)
            .clamp(SOURCE_MIN_WIDTH, SOURCE_MAX_WIDTH);
        let issue = rows
            .iter()
            .map(|row| row.issue.width())
            .chain(std::iter::once("ISSUE".width()))
            .max()
            .unwrap_or(ISSUE_MIN_WIDTH)
            .clamp(ISSUE_MIN_WIDTH, ISSUE_MAX_WIDTH);
        let handling = rows
            .iter()
            .map(|row| row.handling.width())
            .chain(std::iter::once("HANDLING".width()))
            .max()
            .unwrap_or(HANDLING_MIN_WIDTH)
            .clamp(HANDLING_MIN_WIDTH, HANDLING_MAX_WIDTH);

        let mut layout = Self {
            level: LEVEL_WIDTH,
            source,
            issue,
            sources,
            records,
            handling,
        };
        layout.shrink_to(width);
        layout
    }

    fn shrink_to(&mut self, width: usize) {
        let mut excess = self.rendered_width().saturating_sub(width);
        shrink_column(&mut self.issue, ISSUE_MIN_WIDTH, &mut excess);
        shrink_column(&mut self.source, SOURCE_MIN_WIDTH, &mut excess);
        shrink_column(&mut self.handling, HANDLING_MIN_WIDTH, &mut excess);
    }

    fn constraints(self) -> [Constraint; 6] {
        [
            Constraint::Length(self.level as u16),
            Constraint::Length(self.source as u16),
            Constraint::Length(self.issue as u16),
            Constraint::Length(self.sources as u16),
            Constraint::Length(self.records as u16),
            Constraint::Length(self.handling as u16),
        ]
    }

    fn rendered_width(self) -> usize {
        self.level
            + self.source
            + self.issue
            + self.sources
            + self.records
            + self.handling
            + TABLE_COLUMN_SPACING * 5
    }
}

fn shrink_column(column: &mut usize, minimum: usize, excess: &mut usize) {
    let reduction = (*column).saturating_sub(minimum).min(*excess);
    *column -= reduction;
    *excess -= reduction;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokscale_core::source_health::HealthIssueReport;

    use crate::tui::config::TokscaleConfig;
    use crate::tui::themes::ThemeName;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn issue_report() -> HealthReport {
        TokscaleConfig::initialize_default_for_tests();
        HealthReport {
            complete: false,
            clean_sources: 10_190,
            degraded_sources: 1,
            rejected_records: 1,
            partial_sources: 1,
            failed_sources: 1,
            source_data_bytes: 2_684_354_560,
            issues: vec![
                HealthIssueReport {
                    level: "warning".to_string(),
                    source: "zed".to_string(),
                    issue: "missing-model".to_string(),
                    affected_sources: 1,
                    rejected_records: Some(1),
                    handling: "record-skipped".to_string(),
                },
                HealthIssueReport {
                    level: "error".to_string(),
                    source: "opencode".to_string(),
                    issue: "source-unavailable".to_string(),
                    affected_sources: 1,
                    rejected_records: None,
                    handling: "source-skipped".to_string(),
                },
                HealthIssueReport {
                    level: "error".to_string(),
                    source: "claude".to_string(),
                    issue: "partial-source".to_string(),
                    affected_sources: 1,
                    rejected_records: None,
                    handling: "confirmed-data-kept".to_string(),
                },
            ],
        }
    }

    #[test]
    fn issue_rows_render_compact_records_without_samples_or_paths() {
        let rows = issue_rows(&issue_report());

        assert_eq!(
            rows,
            vec![
                IssueRow {
                    level: "WARN",
                    color: Color::Yellow,
                    source: "Zed Agent".to_string(),
                    issue: "Missing Model".to_string(),
                    affected_sources: 1,
                    rejected_records: Some(1),
                    handling: "Record Skipped",
                },
                IssueRow {
                    level: "ERROR",
                    color: Color::Red,
                    source: "OpenCode".to_string(),
                    issue: "Source Unavailable".to_string(),
                    affected_sources: 1,
                    rejected_records: None,
                    handling: "Source Skipped",
                },
                IssueRow {
                    level: "ERROR",
                    color: Color::Red,
                    source: "Claude".to_string(),
                    issue: "Partial Source".to_string(),
                    affected_sources: 1,
                    rejected_records: None,
                    handling: "Confirmed Data Kept",
                },
            ]
        );
        let rendered = format!("{rows:?}");
        assert!(!rendered.contains("thread bad-1"));
        assert!(!rendered.contains("database is corrupt"));
        assert!(!rendered.contains("/tmp/"));
    }

    #[test]
    fn healthy_report_has_no_issue_rows() {
        assert!(issue_rows(&HealthReport::default()).is_empty());
    }

    #[test]
    fn preaggregated_failure_renders_as_one_visible_row() {
        TokscaleConfig::initialize_default_for_tests();
        let health = HealthReport {
            complete: false,
            failed_sources: 5,
            issues: vec![HealthIssueReport {
                level: "error".to_string(),
                source: "kiro".to_string(),
                issue: "source-unavailable".to_string(),
                affected_sources: 5,
                rejected_records: None,
                handling: "source-skipped".to_string(),
            }],
            ..HealthReport::default()
        };

        assert_eq!(
            issue_rows(&health),
            vec![IssueRow {
                level: "ERROR",
                color: Color::Red,
                source: "Kiro".to_string(),
                issue: "Source Unavailable".to_string(),
                affected_sources: 5,
                rejected_records: None,
                handling: "Source Skipped",
            }]
        );
    }

    #[test]
    fn record_rows_keep_source_and_record_counts_separate() {
        TokscaleConfig::initialize_default_for_tests();
        let health = HealthReport {
            complete: false,
            degraded_sources: 5,
            rejected_records: 37,
            issues: vec![HealthIssueReport {
                level: "warning".to_string(),
                source: "kiro".to_string(),
                issue: "missing-model".to_string(),
                affected_sources: 5,
                rejected_records: Some(37),
                handling: "record-skipped".to_string(),
            }],
            ..HealthReport::default()
        };

        let rows = issue_rows(&health);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].affected_sources, 5);
        assert_eq!(rows[0].rejected_records, Some(37));
        assert_eq!(rows[0].handling, "Record Skipped");
    }

    #[test]
    fn wide_summary_explains_source_and_record_statuses() {
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
        let health = issue_report();

        let lines = summary_lines(&theme, &health, 120);
        assert_eq!(lines.len(), 8);
        assert!(line_text(&lines[0]).contains("99.97%"));
        assert!(line_text(&lines[0])
            .contains("10,190 of 10,193 source units are clean at the latest refresh."));
        assert!(line_text(&lines[1]).contains("2.5 GiB"));
        assert!(line_text(&lines[1]).contains("tokscale cache excluded"));
        assert!(line_text(&lines[2]).is_empty());
        assert!(line_text(&lines[3]).contains("Sources"));
        assert!(line_text(&lines[3]).contains("Clean"));
        assert!(line_text(&lines[3]).contains("Scan completed; no records rejected"));
        assert!(line_text(&lines[4]).contains("Degraded"));
        assert!(line_text(&lines[5]).contains("Partial"));
        assert!(line_text(&lines[6]).contains("Failed"));
        assert!(line_text(&lines[7]).contains("Records"));
        assert!(line_text(&lines[7]).contains("Rejected"));
    }

    #[test]
    fn narrow_summary_hides_explanations_without_hiding_statuses() {
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
        let lines = summary_lines(&theme, &issue_report(), 80);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(rendered.contains("99.97%"));
        assert!(rendered.contains("2.5 GiB"));
        assert!(rendered.contains("Clean"));
        assert!(rendered.contains("Rejected"));
        assert!(!rendered.contains("latest refresh"));
        assert!(!rendered.contains("Scan completed"));
        assert!(!rendered.contains("tokscale cache excluded"));
    }

    #[test]
    fn issue_free_statuses_are_muted_and_near_perfect_health_is_green() {
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
        let lines = summary_lines(&theme, &HealthReport::default(), 120);
        let metric_spans = lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| {
                matches!(
                    span.content.trim(),
                    "Degraded" | "Partial" | "Failed" | "Rejected"
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(metric_spans.len(), 4);
        assert!(metric_spans
            .iter()
            .all(|span| span.style.fg == Some(theme.muted)));

        let failed_lines = summary_lines(&theme, &issue_report(), 120);
        let percentage = failed_lines[0]
            .spans
            .iter()
            .find(|span| span.content.contains('%'))
            .unwrap();
        assert_eq!(percentage.style.bg, Some(Color::Green));
    }

    #[test]
    fn health_and_disk_descriptions_start_in_the_same_column() {
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
        let lines = summary_lines(&theme, &issue_report(), 120);
        let description_start = |line: &Line<'_>| {
            line.spans
                .iter()
                .take(3)
                .map(|span| span.content.width())
                .sum::<usize>()
        };

        assert_eq!(description_start(&lines[0]), description_start(&lines[1]));
    }

    #[test]
    fn source_health_percentage_uses_clean_sources_over_all_sources() {
        assert_eq!(source_health_percentage(&HealthReport::default()), "—");
        assert_eq!(
            source_health_percentage(&HealthReport {
                clean_sources: 3,
                ..HealthReport::default()
            }),
            "100%"
        );
        assert_eq!(
            source_health_percentage(&HealthReport {
                clean_sources: 3,
                degraded_sources: 1,
                ..HealthReport::default()
            }),
            "75.00%"
        );
    }

    #[test]
    fn source_health_color_follows_percentage_bands() {
        let theme = Theme::from_name_for_current_terminal(ThemeName::Blue);
        let health = |clean_sources, degraded_sources| HealthReport {
            clean_sources,
            degraded_sources,
            ..HealthReport::default()
        };

        assert_eq!(
            source_health_color(&theme, &health(99, 1), 100),
            Color::Green
        );
        assert_eq!(
            source_health_color(&theme, &health(95, 5), 100),
            Color::Yellow
        );
        assert_eq!(source_health_color(&theme, &health(94, 6), 100), Color::Red);
    }

    #[test]
    fn source_data_size_uses_binary_disk_units() {
        assert_eq!(format_source_data_bytes(0), "0 B");
        assert_eq!(format_source_data_bytes(1_023), "1,023 B");
        assert_eq!(format_source_data_bytes(1_536), "1.5 KiB");
        assert_eq!(format_source_data_bytes(2_684_354_560), "2.5 GiB");
    }

    #[test]
    fn details_table_layout_is_compact_and_shrinks_descriptive_columns() {
        let rows = issue_rows(&issue_report());

        let wide = IssueTableLayout::for_rows(160, &rows);
        assert_eq!(wide.rendered_width(), 75);
        assert_eq!(
            details_table_area(Rect::new(4, 2, 160, 20), wide).width,
            110
        );

        let medium = IssueTableLayout::for_rows(60, &rows);
        assert_eq!(medium.rendered_width(), 60);
        assert_eq!(
            details_table_area(Rect::new(4, 2, 60, 20), medium).width,
            60
        );
        assert!(medium.issue < wide.issue);
        assert!(medium.source < wide.source);
        assert!(medium.handling < wide.handling);

        let narrow = IssueTableLayout::for_rows(51, &rows);
        assert_eq!(narrow.rendered_width(), 51);
        assert_eq!(narrow.issue, ISSUE_MIN_WIDTH);
        assert_eq!(narrow.source, SOURCE_MIN_WIDTH);
        assert_eq!(narrow.handling, HANDLING_MIN_WIDTH);
    }

    #[test]
    fn details_panel_height_tracks_content_and_caps_at_available_height() {
        let area = Rect::new(2, 3, 120, 40);

        let [summary, details] = issue_panel_areas(area, 8, 3);
        assert_eq!(summary.height, 10);
        assert_eq!(details.y, 13);
        assert_eq!(details.height, 6);
        assert!(details.bottom() < area.bottom());

        let [_, empty_details] = issue_panel_areas(area, 8, 0);
        assert_eq!(empty_details.height, 3);

        let [_, overflowing_details] = issue_panel_areas(area, 8, 100);
        assert_eq!(overflowing_details.height, 30);
        assert_eq!(overflowing_details.bottom(), area.bottom());
    }
}
