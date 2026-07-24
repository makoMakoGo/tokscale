//! Chromeless stacked bar chart for the Overview "Tokens per Day" panel.
//!
//! Rendering contract (implemented in this module):
//! - `area` is the whole chart content area inside a bordered box drawn by the
//!   caller: there is no y-axis gutter and no title row, so bars span the full
//!   width edge to edge.
//! - Rows relative to `area`: bars occupy rows `0..h-2`, row `h-2` is a
//!   baseline of '─', row `h-1` holds the date labels.
//! - A single dotted gridline ('┄') crosses the vertical middle of the bar
//!   field; it is drawn before the bars so it only shows through empty cells.
//! - A compact `peak {max}` marker overlays the top-right of bar row 0.
//! - Date labels are anchored to the edges: first date left-aligned, last date
//!   right-aligned, middle date centered when there is room.

use ratatui::prelude::*;

use super::widgets::format_tokens;
use crate::tui::app::App;

/// 8-level block characters for sub-cell precision (matching OpenTUI)
const BLOCKS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

const MONTH_NAMES: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A single model's contribution to a bar
#[derive(Debug, Clone)]
pub struct ModelSegment {
    pub model_id: String,
    pub tokens: u64,
    pub color: Color,
}

/// Data for a single bar in the stacked chart
#[derive(Debug, Clone)]
pub struct StackedBarData {
    pub date: String,
    pub models: Vec<ModelSegment>,
    pub total: u64,
}

/// Render a stacked bar chart where each bar shows model breakdown
pub fn render_stacked_bar_chart(frame: &mut Frame, app: &App, area: Rect, data: &[StackedBarData]) {
    if data.is_empty() || area.height < 2 || area.width == 0 {
        return;
    }

    let chart_width = area.width as usize;
    let chart_height = area.height.saturating_sub(2) as usize;

    let max_value = data
        .iter()
        .map(|d| d.total as f64)
        .fold(0.0_f64, |a, b| a.max(b))
        .max(1.0);

    let buf = frame.buffer_mut();
    let bar_count = data.len();

    let get_bar_width = |index: usize| -> usize {
        if bar_count == 0 {
            return 1;
        }
        let start = (index * chart_width) / bar_count;
        let end = ((index + 1) * chart_width) / bar_count;
        (end - start).max(1)
    };

    // Dotted mid gridline, drawn before the bars so they overwrite it and it
    // only shows through the empty cells above shorter bars.
    if chart_height >= 4 {
        let grid_y = area.y + (chart_height / 2) as u16;
        for x in area.x..area.x + area.width {
            buf[(x, grid_y)]
                .set_char('┄')
                .set_style(Style::default().fg(app.theme.visualization.grid));
        }
    }

    // Render bars row by row (from top to bottom visually, which is high values to low)
    for row_from_bottom in (0..chart_height).rev() {
        let row_index = chart_height - 1 - row_from_bottom;
        let y = area.y + row_index as u16;

        // Render each bar
        let mut x_pos = area.x;
        for (bar_index, bar_data) in data.iter().enumerate() {
            let bar_width = get_bar_width(bar_index);

            let row_threshold = ((row_from_bottom + 1) as f64 / chart_height as f64) * max_value;
            let prev_threshold = (row_from_bottom as f64 / chart_height as f64) * max_value;
            let threshold_diff = row_threshold - prev_threshold;

            let total = bar_data.total as f64;

            // Get the character and color for this cell using stacked model logic
            let (ch, fg_color) = get_stacked_bar_content(
                bar_data,
                total,
                row_threshold,
                prev_threshold,
                threshold_diff,
                app.theme.text.muted,
                app.theme.visualization.chart_highlight,
            );

            for _ in 0..bar_width {
                if x_pos < area.x + area.width {
                    // Leave empty cells untouched so the gridline shows through.
                    if ch != ' ' {
                        buf[(x_pos, y)].set_char(ch).set_fg(fg_color);
                    }
                    x_pos += 1;
                }
            }
        }
    }

    // Baseline separating the bars from the date labels
    let baseline_y = area.y + area.height - 2;
    for x in area.x..area.x + area.width {
        buf[(x, baseline_y)]
            .set_char('─')
            .set_style(Style::default().fg(app.theme.visualization.grid));
    }

    // Compact peak marker, right-aligned on the top bar row; drawn after the
    // bars with an explicit background so it reads over any bar beneath it.
    if chart_height > 0 {
        let peak_label = format!("peak {}", compact_tokens(max_value as u64));
        let peak_width = peak_label.chars().count() as u16;
        if area.width >= peak_width + 2 {
            let peak_x = area.x + area.width - peak_width;
            for (i, ch) in peak_label.chars().enumerate() {
                buf[(peak_x + i as u16, area.y)].set_char(ch).set_style(
                    Style::default()
                        .fg(app.theme.text.muted)
                        .bg(app.theme.surface.panel),
                );
            }
        }
    }

    render_date_labels(buf, app, area, data);
}

/// Date labels anchored to the edges of the chart: first date left-aligned at
/// `area.x`, last date right-aligned to end at the right edge, and — unless the
/// app is very narrow — the middle date centered. When the labels would
/// overlap, the middle one is dropped first; if the remaining two still
/// collide, the right one is truncated from the left, keeping its tail.
fn render_date_labels(buf: &mut Buffer, app: &App, area: Rect, data: &[StackedBarData]) {
    let label_y = area.y + area.height - 1;
    let is_very_narrow = app.is_very_narrow();
    let bar_count = data.len();

    let first_label = format_date_label(&data[0].date, is_very_narrow);
    let first_width = first_label.chars().count() as u16;

    let mut labels: Vec<(String, u16)> = vec![(first_label, area.x)];

    if bar_count > 1 {
        let last_label = format_date_label(&data[bar_count - 1].date, is_very_narrow);
        let last_width = last_label.chars().count() as u16;

        if !is_very_narrow && bar_count > 2 {
            let middle_label = format_date_label(&data[bar_count / 2].date, is_very_narrow);
            let middle_width = middle_label.chars().count() as u16;
            // Keep the middle label only when all three fit with a gap each.
            if first_width + middle_width + last_width + 2 <= area.width {
                let middle_x = area.x + (area.width - middle_width) / 2;
                labels.push((middle_label, middle_x));
            }
        }

        let last_label = if first_width + last_width + 1 > area.width {
            // Still colliding without the middle label: truncate the right
            // label from the left, keeping its tail.
            let keep = area.width.saturating_sub(first_width + 1) as usize;
            last_label
                .chars()
                .skip(last_label.chars().count().saturating_sub(keep))
                .collect()
        } else {
            last_label
        };
        let last_width = last_label.chars().count() as u16;
        if last_width > 0 {
            labels.push((last_label, area.x + area.width - last_width));
        }
    }

    for (label, label_x) in labels {
        for (i, ch) in label.chars().enumerate() {
            let x = label_x + i as u16;
            if x < area.x + area.width {
                buf[(x, label_y)]
                    .set_char(ch)
                    .set_style(Style::default().fg(app.theme.text.muted));
            }
        }
    }
}

/// Format a raw `month/day` date string for the label row.
fn format_date_label(date_str: &str, is_very_narrow: bool) -> String {
    if let Some((month_str, day_str)) = date_str.split_once('/') {
        if let (Ok(month), Ok(day)) = (month_str.parse::<usize>(), day_str.parse::<u32>()) {
            if (1..=12).contains(&month) {
                return if is_very_narrow {
                    format!("{}/{}", month, day)
                } else {
                    format!("{} {}", MONTH_NAMES[month - 1], day)
                };
            }
        }
    }
    date_str.to_string()
}

/// Compact token count for the peak marker: when the integer part has 2+
/// digits, drop the decimal ("38.2B" -> "38B", "123.4B" -> "123B"; "2.1B"
/// stays as-is).
fn compact_tokens(tokens: u64) -> String {
    let formatted = format_tokens(tokens);
    if let Some(dot) = formatted.find('.') {
        let int_digits = formatted[..dot]
            .chars()
            .filter(|c| c.is_ascii_digit())
            .count();
        if int_digits >= 2 {
            let unit = formatted[dot + 1..].trim_start_matches(|c: char| c.is_ascii_digit());
            return format!("{}{}", &formatted[..dot], unit);
        }
    }
    formatted
}

fn get_stacked_bar_content(
    bar_data: &StackedBarData,
    total: f64,
    row_threshold: f64,
    prev_threshold: f64,
    threshold_diff: f64,
    muted_color: Color,
    fallback_color: Color,
) -> (char, Color) {
    if total <= prev_threshold {
        return (' ', muted_color);
    }

    if bar_data.models.is_empty() {
        return (' ', muted_color);
    }

    // Note: Sorting happens per cell render. If performance becomes an issue,
    // consider pre-sorting the model list before calling this function.
    let mut sorted_models: Vec<&ModelSegment> = bar_data.models.iter().collect();
    sorted_models.sort_by(|a, b| a.model_id.cmp(&b.model_id));

    let row_start = prev_threshold;
    let row_end = row_threshold;

    let mut current_height: f64 = 0.0;
    let mut max_overlap: f64 = 0.0;
    let mut best_color = sorted_models
        .first()
        .map(|m| m.color)
        .unwrap_or(fallback_color);

    for model in &sorted_models {
        let m_start = current_height;
        let m_end = current_height + model.tokens as f64;
        current_height += model.tokens as f64;

        let overlap_start = m_start.max(row_start);
        let overlap_end = m_end.min(row_end);
        let overlap = (overlap_end - overlap_start).max(0.0);

        if overlap > max_overlap {
            max_overlap = overlap;
            best_color = model.color;
        }
    }

    if total >= row_threshold {
        return (BLOCKS[8], best_color);
    }

    let ratio = if threshold_diff > 0.0 {
        (total - prev_threshold) / threshold_diff
    } else {
        1.0
    };
    let block_index = (ratio * 8.0).floor().clamp(1.0, 8.0) as usize;
    (BLOCKS[block_index], best_color)
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
        app.terminal_width = width;
        app
    }

    fn bar(date: &str, total: u64) -> StackedBarData {
        StackedBarData {
            date: date.to_string(),
            models: vec![ModelSegment {
                model_id: "test-model".to_string(),
                tokens: total,
                color: Color::Green,
            }],
            total,
        }
    }

    fn render_chart(
        app: &App,
        area: Rect,
        data: &[StackedBarData],
        width: u16,
        height: u16,
    ) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_stacked_bar_chart(frame, app, area, data))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_string(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn bars_touch_both_edges_of_the_area() {
        let app = make_app(120);
        let data: Vec<StackedBarData> = (0..7).map(|i| bar("1/5", 100 + i)).collect();
        let area = Rect::new(2, 1, 40, 10);
        let buf = render_chart(&app, area, &data, 44, 12);

        // The tallest bar reaches the top row; every row of the bar field
        // starts at area.x and ends at the right edge.
        let bar_rows = area.y..area.y + area.height - 2;
        for y in bar_rows.clone() {
            assert_ne!(
                buf[(area.x, y)].symbol(),
                " ",
                "left edge of row {y} should be covered by a bar"
            );
            assert_ne!(
                buf[(area.x + area.width - 1, y)].symbol(),
                " ",
                "right edge of row {y} should be covered by a bar"
            );
        }
        // No y-axis gutter: the cell just left of the area stays untouched.
        for y in bar_rows {
            assert_eq!(buf[(area.x - 1, y)].symbol(), " ");
        }
    }

    #[test]
    fn baseline_row_is_all_horizontal_lines() {
        let app = make_app(120);
        let data = vec![bar("1/5", 100), bar("1/6", 0), bar("1/7", 50)];
        let area = Rect::new(0, 0, 30, 8);
        let buf = render_chart(&app, area, &data, 30, 8);

        let baseline_y = area.y + area.height - 2;
        for x in area.x..area.x + area.width {
            assert_eq!(buf[(x, baseline_y)].symbol(), "─");
            assert_eq!(buf[(x, baseline_y)].fg, app.theme.visualization.grid);
        }
    }

    #[test]
    fn peak_label_sits_top_right_and_compacts_the_value() {
        let app = make_app(120);
        let data = vec![bar("1/5", 38_200_000_000), bar("1/6", 100)];
        let area = Rect::new(0, 0, 30, 8);
        let buf = render_chart(&app, area, &data, 30, 8);

        let label = "peak 38B";
        let start_x = area.x + area.width - label.len() as u16;
        let row: String = row_string(&buf, area.y)
            .chars()
            .skip(start_x as usize)
            .collect();
        assert_eq!(row, label);
        assert_eq!(buf[(start_x, area.y)].fg, app.theme.text.muted);
        assert_eq!(buf[(start_x, area.y)].bg, app.theme.surface.panel);
    }

    #[test]
    fn compact_tokens_drops_decimals_for_two_or_more_integer_digits() {
        assert_eq!(compact_tokens(38_200_000_000), "38B");
        assert_eq!(compact_tokens(123_400_000_000), "123B");
        assert_eq!(compact_tokens(2_100_000_000), "2.1B");
        assert_eq!(compact_tokens(500), "500");
    }

    #[test]
    fn mid_gridline_shows_only_through_empty_cells() {
        let app = make_app(120);
        // Left half full height, right half empty.
        let data = vec![bar("1/5", 100), bar("1/6", 0)];
        let area = Rect::new(0, 0, 20, 10);
        let buf = render_chart(&app, area, &data, 20, 10);

        let chart_height = area.height - 2;
        let grid_y = area.y + chart_height / 2;
        // Full bar overwrites the gridline on the left half.
        assert_eq!(buf[(2, grid_y)].symbol(), "█");
        // Empty bar lets the gridline show through on the right half.
        assert_eq!(buf[(15, grid_y)].symbol(), "┄");
        assert_eq!(buf[(15, grid_y)].fg, app.theme.visualization.grid);
    }

    #[test]
    fn date_labels_anchor_to_the_edges() {
        let app = make_app(120);
        let data = vec![bar("1/5", 10), bar("6/15", 20), bar("12/25", 30)];
        let area = Rect::new(3, 2, 40, 8);
        let buf = render_chart(&app, area, &data, 46, 12);

        let label_y = area.y + area.height - 1;
        let row = row_string(&buf, label_y);
        let inner: String = row
            .chars()
            .skip(area.x as usize)
            .take(area.width as usize)
            .collect();
        assert!(
            inner.starts_with("Jan 5"),
            "first date at area.x: {inner:?}"
        );
        assert!(
            inner.ends_with("Dec 25"),
            "last date at right edge: {inner:?}"
        );
        assert!(inner.contains("Jun 15"), "middle date centered: {inner:?}");
        let middle_start = inner.find("Jun 15").unwrap();
        let expected = (area.width as usize - "Jun 15".len()) / 2;
        assert!(
            middle_start.abs_diff(expected) <= 1,
            "middle label roughly centered at {middle_start}, expected ~{expected}"
        );
    }

    #[test]
    fn very_narrow_app_renders_only_two_labels() {
        let app = make_app(50);
        let data = vec![bar("1/5", 10), bar("6/15", 20), bar("12/25", 30)];
        let area = Rect::new(0, 0, 30, 8);
        let buf = render_chart(&app, area, &data, 30, 8);

        let label_y = area.y + area.height - 1;
        let inner = row_string(&buf, label_y);
        assert!(inner.starts_with("1/5"), "first date at area.x: {inner:?}");
        assert!(
            inner.ends_with("12/25"),
            "last date at right edge: {inner:?}"
        );
        assert!(
            !inner.contains("6/15"),
            "very narrow apps drop the middle label: {inner:?}"
        );
    }

    #[test]
    fn tiny_and_empty_inputs_do_not_panic() {
        let app = make_app(120);
        let data = vec![bar("1/5", 100), bar("1/6", 50)];
        for (width, height) in [(0, 0), (1, 0), (0, 1), (1, 1), (4, 1), (3, 2), (1, 5)] {
            let _ = render_chart(&app, Rect::new(0, 0, width, height), &data, 8, 8);
        }
        let _ = render_chart(&app, Rect::new(0, 0, 8, 8), &[], 8, 8);
    }
}
