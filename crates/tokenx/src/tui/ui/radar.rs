use ratatui::prelude::*;
use ratatui::symbols::Marker;
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine, Points};
use ratatui::widgets::Paragraph;

use super::widgets::truncate_model_display_name_to;

/// One axis of the day radar: a display label plus its 0..=1 share of the day.
#[derive(Debug)]
pub struct RadarAxis {
    pub label: String,
    pub share: f64,
}

const CENTER: f64 = 50.0;
const BOUNDS: f64 = 100.0;
const FILL_SAMPLES: usize = 24;
const LABEL_MAX_WIDTH: usize = 18;
const CHART_MIN_W: u16 = 12;

/// GitHub-style 4-axis radar. Axes are fixed to top/right/bottom/left so the
/// chart stays comparable across days. The braille chart is square and
/// centered in the area; each axis has a two-line pct/name caption hugging
/// its tip, so labels stay attached to the cross at any size.
pub fn render_radar(
    frame: &mut Frame,
    area: Rect,
    axes: &[RadarAxis; 4],
    accent: Color,
    secondary: Color,
    fill: Color,
    background: Color,
) {
    if area.width < 20 || area.height < 9 {
        return;
    }

    // GitHub-style two-line captions: pct on the outer line, name on the
    // inner line, each pair a centered block hugging its axis tip.
    let pct_text = |axis: &RadarAxis| format!("{:.0}%", axis.share.clamp(0.0, 1.0) * 100.0);
    let name_text = |axis: &RadarAxis, budget: usize| {
        if axis.label.is_empty() {
            String::new()
        } else {
            truncate_model_display_name_to(&axis.label, budget.max(1))
        }
    };
    let side_budget = ((area.width as usize) / 4).clamp(6, LABEL_MAX_WIDTH);
    let names = [
        name_text(&axes[0], LABEL_MAX_WIDTH),
        name_text(&axes[1], side_budget),
        name_text(&axes[2], LABEL_MAX_WIDTH),
        name_text(&axes[3], side_budget),
    ];
    let pcts = [
        pct_text(&axes[0]),
        pct_text(&axes[1]),
        pct_text(&axes[2]),
        pct_text(&axes[3]),
    ];

    // The chart is square (two cells per row of height keeps braille dots
    // square) and centered in the zone; the side captions occupy the flanks.
    let text_w = |text: &str| text.chars().count() as f64;
    let flank = text_w(&names[1])
        .max(text_w(&pcts[1]))
        .max(text_w(&names[3]))
        .max(text_w(&pcts[3]))
        + 1.0;
    let chart_w = (area.height * 2).min(area.width.saturating_sub(flank as u16 * 2));
    if chart_w < CHART_MIN_W {
        return;
    }
    // Cap the height as well so the braille dot grid stays square even when
    // the caption flanks bind the width; center the square in the area.
    let chart_h = (chart_w / 2).min(area.height);
    let chart_x = area.x + (area.width - chart_w) / 2;
    let chart_y = area.y + (area.height - chart_h) / 2;
    let chart = Rect::new(chart_x, chart_y, chart_w, chart_h);

    // Axis tips stop just inside the square, clear of the captions. Both
    // arms share one length so the cross stays square in braille dots.
    let x_arm = (CENTER - 1.5 / f64::from(chart_w) * BOUNDS).clamp(15.0, 42.0);
    let y_arm = (CENTER - 2.5 / f64::from(chart_h) * BOUNDS).clamp(15.0, 42.0);
    let arm = x_arm.min(y_arm);
    let (x_arm, y_arm) = (arm, arm);

    // Endpoint directions in logical units (canvas y grows upward), arms in
    // axis order: top, right, bottom, left.
    let directions = [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)];
    let arms = [y_arm, x_arm, y_arm, x_arm];
    let points: Vec<(f64, f64)> = directions
        .iter()
        .zip(arms.iter())
        .zip(axes.iter())
        .map(|(((dx, dy), arm), axis)| {
            let share = if axis.share.is_finite() {
                axis.share.clamp(0.0, 1.0)
            } else {
                0.0
            };
            (CENTER + dx * share * arm, CENTER + dy * share * arm)
        })
        .collect();
    let centroid = (
        points.iter().map(|p| p.0).sum::<f64>() / 4.0,
        points.iter().map(|p| p.1).sum::<f64>() / 4.0,
    );

    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .background_color(background)
        .x_bounds([0.0, BOUNDS])
        .y_bounds([0.0, BOUNDS])
        .paint(|ctx| {
            for ((dx, dy), arm) in directions.iter().zip(arms.iter()) {
                ctx.draw(&CanvasLine {
                    x1: CENTER,
                    y1: CENTER,
                    x2: CENTER + dx * arm,
                    y2: CENTER + dy * arm,
                    color: secondary,
                });
            }

            // Fan-fill the polygon: centroid to samples along each edge.
            for i in 0..4 {
                let (ax, ay) = points[i];
                let (bx, by) = points[(i + 1) % 4];
                for step in 0..=FILL_SAMPLES {
                    let t = step as f64 / FILL_SAMPLES as f64;
                    ctx.draw(&CanvasLine {
                        x1: centroid.0,
                        y1: centroid.1,
                        x2: ax + (bx - ax) * t,
                        y2: ay + (by - ay) * t,
                        color: fill,
                    });
                }
            }

            for i in 0..4 {
                let (ax, ay) = points[i];
                let (bx, by) = points[(i + 1) % 4];
                ctx.draw(&CanvasLine {
                    x1: ax,
                    y1: ay,
                    x2: bx,
                    y2: by,
                    color: accent,
                });
            }
            ctx.draw(&Points {
                coords: &points,
                color: accent,
            });
        });

    frame.render_widget(canvas, chart);

    // Captions: pct/name pairs hugging each axis tip (GitHub style).
    let label_style = Style::default().fg(secondary);
    let print = |frame: &mut Frame, x: u16, y: u16, text: &str| {
        if !text.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(text.to_string(), label_style))),
                Rect::new(x, y, text.chars().count() as u16, 1),
            );
        }
    };
    let tip_x = |logical_x: f64| chart.x + (logical_x / BOUNDS * f64::from(chart.width)) as u16;
    let center_x = chart.x + chart.width / 2;
    let mid_y = chart.y + chart.height / 2;
    let centered = |text: &str| {
        center_x
            .saturating_sub(text.chars().count() as u16 / 2)
            .max(area.x)
    };

    // Top: pct over name, both centered above the tip. Bottom: pct under the
    // tip, name at the very bottom. Axes without a name are skipped.
    if !names[0].is_empty() {
        print(frame, centered(&pcts[0]), chart.y, &pcts[0]);
        print(frame, centered(&names[0]), chart.y + 1, &names[0]);
    }
    if !names[2].is_empty() {
        print(
            frame,
            centered(&pcts[2]),
            chart.y + chart.height - 2,
            &pcts[2],
        );
        print(
            frame,
            centered(&names[2]),
            chart.y + chart.height - 1,
            &names[2],
        );
    }

    // Sides: pct and name straddle the horizontal axis row (pct above, name
    // below), centered as a block one cell outside the tip.
    let pct_y = mid_y.saturating_sub(1);
    let name_y = (mid_y + 1).min(chart.y + chart.height - 1);
    let print_side = |frame: &mut Frame, block_x: u16, name: &str, pct: &str| {
        if name.is_empty() {
            return;
        }
        let block_w = name.chars().count().max(pct.chars().count()) as u16;
        print(
            frame,
            block_x + (block_w - pct.chars().count() as u16) / 2,
            pct_y,
            pct,
        );
        print(
            frame,
            block_x + (block_w - name.chars().count() as u16) / 2,
            name_y,
            name,
        );
    };
    let block_w = |name: &str, pct: &str| name.chars().count().max(pct.chars().count()) as u16;

    let left_x = tip_x(CENTER - x_arm)
        .saturating_sub(block_w(&names[3], &pcts[3]) + 1)
        .max(area.x);
    print_side(frame, left_x, &names[3], &pcts[3]);

    let right_w = block_w(&names[1], &pcts[1]);
    let right_x = (tip_x(CENTER + x_arm) + 1).min(area.x + area.width.saturating_sub(right_w));
    print_side(frame, right_x, &names[1], &pcts[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn sample_axes() -> [RadarAxis; 4] {
        [
            RadarAxis {
                label: "alpha".into(),
                share: 0.5,
            },
            RadarAxis {
                label: "beta".into(),
                share: 0.3,
            },
            RadarAxis {
                label: "gamma".into(),
                share: 0.15,
            },
            RadarAxis {
                label: "Others".into(),
                share: 0.05,
            },
        ]
    }

    fn find_row(buf: &ratatui::buffer::Buffer, area: Rect, needle: &str) -> Option<u16> {
        (area.y..area.y + area.height).find(|&y| {
            (area.x..area.x + area.width)
                .map(|x| buf.cell((x, y)).unwrap().symbol())
                .collect::<String>()
                .contains(needle)
        })
    }

    #[test]
    fn chart_height_is_capped_to_stay_square_when_width_binds() {
        // 26x24 area: side flanks take 7 columns each, so chart_w = 12 and
        // the square chart is 12x6, vertically centered (chart_y = 9) instead
        // of stretched to the full 24 rows.
        let area = Rect::new(0, 0, 26, 24);
        let axes = sample_axes();
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        let frame = terminal
            .draw(|f| {
                render_radar(
                    f,
                    area,
                    &axes,
                    Color::Cyan,
                    Color::DarkGray,
                    Color::Green,
                    Color::Black,
                )
            })
            .unwrap();
        let buf = frame.buffer;

        // Top pct at chart.y, top name at chart.y + 1.
        assert_eq!(find_row(buf, area, "alpha"), Some(10));
        // Bottom pct at chart.y + chart_h - 2, bottom name one row lower.
        assert_eq!(find_row(buf, area, "gamma"), Some(14));
        // Side names straddle the horizontal axis row.
        assert_eq!(find_row(buf, area, "beta"), Some(13));
        assert_eq!(find_row(buf, area, "Others"), Some(13));
    }

    #[test]
    fn tiny_area_is_skipped_without_panicking() {
        let axes = sample_axes();
        let mut terminal = Terminal::new(TestBackend::new(19, 8)).unwrap();
        terminal
            .draw(|f| {
                render_radar(
                    f,
                    Rect::new(0, 0, 19, 8),
                    &axes,
                    Color::Cyan,
                    Color::DarkGray,
                    Color::Green,
                    Color::Black,
                )
            })
            .unwrap();
    }
}
