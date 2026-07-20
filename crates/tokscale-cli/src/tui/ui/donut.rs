//! Donut (ring) chart widget used by the Overview snapshot panel.
//!
//! Rendering contract (implemented in this module):
//! - Segments are laid out clockwise starting at 12 o'clock, in slice order.
//! - The ring is drawn as a braille dot matrix: each cell holds a 2x4 dot
//!   grid (Unicode braille, base U+2800), so unset dots leave the background
//!   visible and the ring reads as a separated-dot texture.
//! - Braille dots are physically ~square (a ~1:2 cell split into 2x4 dots),
//!   so the ring is evaluated as a true circle in dot space with no aspect
//!   correction.
//! - A cell can only have one foreground color: when its set dots span two
//!   segments, the majority color wins and ties go to the segment that
//!   appears earlier in slice order.
//! - Every non-zero segment is guaranteed a minimum visible arc (see
//!   `MIN_VISIBLE_DOTS`); this is a deliberate visualization normalization —
//!   the legend always shows the exact values, the ring guarantees visibility.
//! - `center` lines are horizontally and vertically centered inside the hole on
//!   a cleared background.
//! - When every segment weighs zero (or there are no segments), the ring is
//!   drawn uniformly in `empty_color`.

use ratatui::prelude::*;
use ratatui::widgets::Widget;
use std::f64::consts::TAU;

/// First code point of the Unicode braille pattern block; each cell's glyph is
/// `BRAILLE_BASE` plus the bit mask of its set dots.
const BRAILLE_BASE: u32 = 0x2800;
/// Bit mask of each dot in a cell's 2x4 braille grid, rows top to bottom.
/// Braille numbers the left column dots 1, 2, 3, 7 and the right column dots
/// 4, 5, 6, 8.
const BRAILLE_DOTS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
/// Ring band thickness relative to the outer radius (chunky band like the mock).
const INNER_RADIUS_RATIO: f64 = 0.62;
/// Inset in ring dots so edge dots are not clipped by the area boundary.
const EDGE_INSET: f64 = 0.5;
/// Minimum arc length, in dots on the mid-band circumference, guaranteed for
/// every non-zero segment so tiny buckets stay visible on the ring.
const MIN_VISIBLE_DOTS: f64 = 4.0;
/// Cap on the minimum visible arc as a fraction of the ring, so tiny rings
/// don't over-distort segment proportions.
const MIN_VISIBLE_FRACTION_MAX: f64 = 0.05;

/// One proportional, colored arc of a donut chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DonutSegment {
    /// Relative weight of the arc; zero-weight segments are not drawn.
    pub value: u64,
    /// Color used for the arc and its legend marker.
    pub color: Color,
}

impl DonutSegment {
    pub fn new(value: u64, color: Color) -> Self {
        Self { value, color }
    }
}

/// A terminal ring chart; see the module docs for the rendering contract.
pub struct DonutChart {
    pub segments: Vec<DonutSegment>,
    pub center: Vec<Line<'static>>,
    pub background: Color,
    pub empty_color: Color,
    /// Optional cap on the ring's outer radius measured in terminal rows; the
    /// ring stays centered when capped.
    pub max_radius: Option<f64>,
}

impl DonutChart {
    pub fn new(segments: Vec<DonutSegment>) -> Self {
        Self {
            segments,
            center: Vec::new(),
            background: Color::Reset,
            empty_color: Color::DarkGray,
            max_radius: None,
        }
    }

    pub fn center(mut self, lines: Vec<Line<'static>>) -> Self {
        self.center = lines;
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.background = color;
        self
    }

    pub fn empty_color(mut self, color: Color) -> Self {
        self.empty_color = color;
        self
    }

    pub fn max_radius(mut self, radius: f64) -> Self {
        self.max_radius = Some(radius);
        self
    }

    /// Write the center lines into the ring hole, centered inside `area`.
    fn render_center(&self, area: Rect, buf: &mut Buffer) {
        if self.center.is_empty() {
            return;
        }
        let text_w = self.center.iter().map(Line::width).max().unwrap_or(0) as u16;
        let text_h = self.center.len() as u16;
        let box_w = text_w.min(area.width);
        let box_h = text_h.min(area.height);
        if box_w == 0 || box_h == 0 {
            return;
        }
        let start_x = area.x + (area.width - box_w) / 2;
        let start_y = area.y + (area.height - box_h) / 2;

        // The text wins over ring dots, so clear its box first.
        for y in start_y..start_y + box_h {
            for x in start_x..start_x + box_w {
                let cell = &mut buf[(x, y)];
                cell.set_char(' ');
                cell.set_bg(self.background);
            }
        }
        for (row, line) in self.center.iter().take(box_h as usize).enumerate() {
            buf.set_line(start_x, start_y + row as u16, line, box_w);
        }
    }
}

/// Guarantee a minimum visible arc for every non-zero segment.
///
/// Fractions below `min_fraction` are bumped up to it and the total bump is
/// subtracted from the segments strictly above the threshold, proportional to
/// their current fractions, so the fractions still sum to 1. Segments sitting
/// exactly at the threshold never fund the bump, so they are not dragged back
/// below it. Zero fractions stay zero: invisible data stays invisible. A
/// shrinking segment can cross the threshold, so this iterates, bounded by
/// `fractions.len()` passes; if it cannot converge — e.g. `min_fraction`
/// times the segment count reaches 1 — the non-zero segments fall back to
/// equal fractions.
fn adjust_visible_fractions(fractions: &mut [f64], min_fraction: f64) {
    let count = fractions.iter().filter(|&&f| f > 0.0).count();
    if count == 0 || min_fraction <= 0.0 {
        return;
    }
    if min_fraction * (count as f64) < 1.0 {
        for _ in 0..fractions.len() {
            let bump: f64 = fractions
                .iter()
                .filter(|&&f| f > 0.0 && f < min_fraction)
                .map(|&f| min_fraction - f)
                .sum();
            if bump == 0.0 {
                return;
            }
            let donor_total: f64 = fractions.iter().filter(|&&f| f > min_fraction).sum();
            if donor_total <= bump {
                break;
            }
            for f in fractions.iter_mut() {
                if *f == 0.0 {
                    continue;
                }
                if *f < min_fraction {
                    *f = min_fraction;
                } else if *f > min_fraction {
                    *f -= bump * (*f / donor_total);
                }
            }
        }
        if fractions.iter().all(|&f| f == 0.0 || f >= min_fraction) {
            return;
        }
    }
    // Cannot give every non-zero segment the minimum (too many segments for
    // the ring, or the donors ran dry): spread the ring evenly instead.
    let equal = 1.0 / count as f64;
    for f in fractions.iter_mut() {
        if *f > 0.0 {
            *f = equal;
        }
    }
}

impl Widget for DonutChart {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        // Braille rendering: each cell holds 2 x 4 dots, so the full dot space
        // is (area.width * 2) x (area.height * 4). Braille dots are physically
        // ~square, so the ring is a true circle in dot space.
        let grid_w = f64::from(area.width) * 2.0;
        let grid_h = f64::from(area.height) * 4.0;
        let center_x = grid_w / 2.0;
        let center_y = grid_h / 2.0;
        let outer_r = (grid_w.min(grid_h) / 2.0 - EDGE_INSET).max(0.0);
        // `max_radius` is measured in terminal rows; a row holds 4 dot rows.
        let outer_r = self
            .max_radius
            .map_or(outer_r, |cap| outer_r.min(cap * 4.0));
        let inner_r = outer_r * INNER_RADIUS_RATIO;

        // Cumulative end fraction of each non-zero segment, in slice order.
        // Deliberate visualization normalization: non-zero segments below a
        // minimum visible arc are bumped up to it (funded proportionally by
        // the larger segments) so tiny buckets stay visible on the ring; the
        // legend always shows the exact values.
        let total = self
            .segments
            .iter()
            .map(|segment| segment.value as u128)
            .sum::<u128>();
        let mut boundaries: Vec<(f64, Color)> = Vec::with_capacity(self.segments.len());
        if total > 0 {
            let mut fractions: Vec<f64> = self
                .segments
                .iter()
                .map(|segment| segment.value as f64 / total as f64)
                .collect();
            let r_mid = (outer_r + inner_r) / 2.0;
            if r_mid > 0.0 {
                let min_fraction = (MIN_VISIBLE_DOTS / (TAU * r_mid)).min(MIN_VISIBLE_FRACTION_MAX);
                adjust_visible_fractions(&mut fractions, min_fraction);
            }
            let mut cumulative = 0.0;
            for (segment, fraction) in self.segments.iter().zip(fractions) {
                if fraction == 0.0 {
                    continue;
                }
                cumulative += fraction;
                boundaries.push((cumulative, segment.color));
            }
        }

        // Color of the ring dot at (px, py) in dot space, or None outside the
        // ring band.
        let ring_color = |px: u16, py: u16| -> Option<Color> {
            let dx = f64::from(px) + 0.5 - center_x;
            let dy = f64::from(py) + 0.5 - center_y;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < inner_r || dist > outer_r {
                return None;
            }
            if boundaries.is_empty() {
                return Some(self.empty_color);
            }
            // Angle 0 at 12 o'clock, increasing clockwise.
            let fraction = dx.atan2(-dy).rem_euclid(TAU) / TAU;
            let color = boundaries
                .iter()
                .find(|(end, _)| fraction < *end)
                .or_else(|| boundaries.last())
                .map(|&(_, color)| color)
                .unwrap_or(self.empty_color);
            Some(color)
        };

        for cell_y in 0..area.height {
            for cell_x in 0..area.width {
                let mut bits = 0u8;
                let mut votes: Vec<(Color, u8)> = Vec::new();
                for (dot_y, row) in BRAILLE_DOTS.iter().enumerate() {
                    for (dot_x, &bit) in row.iter().enumerate() {
                        let px = cell_x * 2 + dot_x as u16;
                        let py = cell_y * 4 + dot_y as u16;
                        let Some(color) = ring_color(px, py) else {
                            continue;
                        };
                        bits |= bit;
                        match votes.iter_mut().find(|(voted, _)| *voted == color) {
                            Some((_, count)) => *count += 1,
                            None => votes.push((color, 1)),
                        }
                    }
                }
                if bits == 0 {
                    continue;
                }
                // One fg per cell: the majority color wins, ties go to the
                // segment that appears earlier in slice order (boundaries are
                // built in slice order). The fallback covers the empty ring,
                // where every dot votes `empty_color`.
                let top = votes.iter().map(|(_, count)| *count).max().unwrap_or(0);
                let color = boundaries
                    .iter()
                    .map(|&(_, segment)| segment)
                    .find(|segment| {
                        votes
                            .iter()
                            .any(|(voted, count)| voted == segment && *count == top)
                    })
                    .unwrap_or_else(|| votes[0].0);
                let symbol = char::from_u32(BRAILLE_BASE + u32::from(bits))
                    .expect("braille pattern bits always form a valid char");
                let cell = &mut buf[(area.x + cell_x, area.y + cell_y)];
                cell.set_symbol(symbol.encode_utf8(&mut [0; 4]))
                    .set_fg(color);
            }
        }

        // Runs after the ring so the text always wins over ring dots.
        self.render_center(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn render_chart(chart: DonutChart, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(chart, frame.area()))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// X positions of the non-space cells in a row, left to right.
    fn ring_cells_in_row(buf: &Buffer, y: u16) -> Vec<u16> {
        (0..buf.area.width)
            .filter(|&x| buf[(x, y)].symbol() != " ")
            .collect()
    }

    /// Rows that contain at least one ring cell, top to bottom.
    fn ring_rows(buf: &Buffer) -> Vec<u16> {
        (0..buf.area.height)
            .filter(|&y| !ring_cells_in_row(buf, y).is_empty())
            .collect()
    }

    /// Number of ring cells drawn in each color across the whole buffer.
    fn ring_cell_counts(buf: &Buffer) -> Vec<(Color, usize)> {
        let mut counts: Vec<(Color, usize)> = Vec::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].symbol() == " " {
                    continue;
                }
                let fg = buf[(x, y)].fg;
                match counts.iter_mut().find(|(color, _)| *color == fg) {
                    Some((_, count)) => *count += 1,
                    None => counts.push((fg, 1)),
                }
            }
        }
        counts
    }

    fn cells_of(counts: &[(Color, usize)], color: Color) -> usize {
        counts
            .iter()
            .find(|(counted, _)| *counted == color)
            .map(|(_, count)| *count)
            .unwrap_or(0)
    }

    /// The real token buckets: 91.1% / 7.9% / 0.5% / 0.5%.
    fn real_bucket_chart() -> DonutChart {
        DonutChart::new(vec![
            DonutSegment::new(911, Color::Green),
            DonutSegment::new(79, Color::Red),
            DonutSegment::new(5, Color::Yellow),
            DonutSegment::new(5, Color::Cyan),
        ])
    }

    #[test]
    fn single_segment_ring_reaches_three_and_nine_oclock() {
        let buf = render_chart(
            DonutChart::new(vec![DonutSegment::new(10, Color::Green)]),
            20,
            10,
        );
        let cells = ring_cells_in_row(&buf, 5);
        assert!(!cells.is_empty(), "expected ring cells on the middle row");
        let nine_oclock = *cells.first().unwrap();
        let three_oclock = *cells.last().unwrap();
        assert!(nine_oclock < 10 && three_oclock > 10);
        assert_eq!(buf[(three_oclock, 5)].fg, Color::Green);
        assert_eq!(buf[(nine_oclock, 5)].fg, Color::Green);
        // Cells outside the ring are left untouched for the panel background.
        assert_eq!(buf[(0, 0)].symbol(), " ");
    }

    #[test]
    fn two_equal_segments_split_the_ring_right_and_left() {
        let buf = render_chart(
            DonutChart::new(vec![
                DonutSegment::new(1, Color::Green),
                DonutSegment::new(1, Color::Red),
            ]),
            20,
            10,
        );
        let cells = ring_cells_in_row(&buf, 5);
        assert!(!cells.is_empty(), "expected ring cells on the middle row");
        let nine_oclock = *cells.first().unwrap();
        let three_oclock = *cells.last().unwrap();
        assert_eq!(buf[(three_oclock, 5)].fg, Color::Green);
        assert_eq!(buf[(nine_oclock, 5)].fg, Color::Red);
    }

    #[test]
    fn zero_total_weight_draws_the_whole_ring_in_empty_color() {
        for segments in [
            Vec::new(),
            vec![DonutSegment::new(0, Color::Green)],
            vec![
                DonutSegment::new(0, Color::Green),
                DonutSegment::new(0, Color::Red),
            ],
        ] {
            let buf = render_chart(
                DonutChart::new(segments).empty_color(Color::DarkGray),
                20,
                10,
            );
            let cells = ring_cells_in_row(&buf, 5);
            assert!(!cells.is_empty(), "expected ring cells on the middle row");
            for x in cells {
                assert_eq!(buf[(x, 5)].fg, Color::DarkGray);
            }
        }
    }

    #[test]
    fn zero_weight_segments_are_skipped() {
        let buf = render_chart(
            DonutChart::new(vec![
                DonutSegment::new(1, Color::Green),
                DonutSegment::new(0, Color::Yellow),
                DonutSegment::new(1, Color::Red),
            ]),
            20,
            10,
        );
        let cells = ring_cells_in_row(&buf, 5);
        assert!(!cells.is_empty(), "expected ring cells on the middle row");
        let nine_oclock = *cells.first().unwrap();
        let three_oclock = *cells.last().unwrap();
        assert_eq!(buf[(three_oclock, 5)].fg, Color::Green);
        assert_eq!(buf[(nine_oclock, 5)].fg, Color::Red);
    }

    #[test]
    fn tiny_segments_get_a_minimum_visible_arc() {
        let buf = render_chart(real_bucket_chart(), 30, 14);
        let counts = ring_cell_counts(&buf);
        for color in [Color::Green, Color::Red, Color::Yellow, Color::Cyan] {
            assert!(
                cells_of(&counts, color) > 0,
                "expected {color:?} cells on the ring"
            );
        }
        for color in [Color::Yellow, Color::Cyan] {
            assert!(
                cells_of(&counts, color) >= MIN_VISIBLE_DOTS as usize,
                "tiny segment {color:?} should stay visible, got {} cells",
                cells_of(&counts, color)
            );
        }
    }

    #[test]
    fn big_segment_still_dominates_after_the_adjustment() {
        let buf = render_chart(real_bucket_chart(), 30, 14);
        let counts = ring_cell_counts(&buf);
        let total: usize = counts.iter().map(|(_, count)| count).sum();
        let share = cells_of(&counts, Color::Green) as f64 / total as f64;
        assert!(
            (0.80..=0.93).contains(&share),
            "expected the big segment to keep ~87% of the ring, got {share:.3}"
        );
    }

    #[test]
    fn adjustment_never_resurrects_zero_weight_segments() {
        let mut chart = real_bucket_chart();
        chart.segments.push(DonutSegment::new(0, Color::Magenta));
        let buf = render_chart(chart, 30, 14);
        let counts = ring_cell_counts(&buf);
        assert_eq!(cells_of(&counts, Color::Magenta), 0);
        for color in [Color::Green, Color::Red, Color::Yellow, Color::Cyan] {
            assert!(
                cells_of(&counts, color) > 0,
                "expected {color:?} cells on the ring"
            );
        }
    }

    #[test]
    fn adjusted_ring_has_no_gaps_on_the_mid_row() {
        let buf = render_chart(real_bucket_chart(), 30, 14);
        let cells = ring_cells_in_row(&buf, 7);
        assert!(!cells.is_empty(), "expected ring cells on the middle row");
        // The only gap in the row is the ring hole: the adjusted fractions
        // still sum to 1, so no angle inside the band is left uncolored.
        let gaps = cells
            .windows(2)
            .filter(|pair| pair[1] - pair[0] > 1)
            .count();
        assert_eq!(gaps, 1, "expected exactly the ring hole as a gap");
    }

    #[test]
    fn visible_fraction_adjustment_bumps_donates_and_falls_back() {
        // Tiny non-zero segments are bumped to the minimum, funded
        // proportionally by the bigger segments; fractions still sum to 1.
        let mut fractions = vec![0.911, 0.079, 0.005, 0.005];
        adjust_visible_fractions(&mut fractions, 0.04);
        assert_eq!(fractions[2], 0.04);
        assert_eq!(fractions[3], 0.04);
        assert!(fractions[0] < 0.911 && fractions[0] > 0.8);
        assert!(fractions[1] < 0.079 && fractions[1] > 0.04);
        let sum: f64 = fractions.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "fractions must still sum to 1");

        // Zero fractions stay zero: invisible data stays invisible.
        let mut fractions = vec![0.95, 0.0, 0.05];
        adjust_visible_fractions(&mut fractions, 0.1);
        assert_eq!(fractions[1], 0.0);
        assert_eq!(fractions[2], 0.1);

        // Too many segments for the minimum: fall back to equal fractions.
        let mut fractions = vec![0.7, 0.1, 0.1, 0.05, 0.05];
        adjust_visible_fractions(&mut fractions, 0.3);
        assert!(fractions.iter().all(|&f| f == 0.2));
    }

    #[test]
    fn every_ring_cell_is_a_braille_pattern() {
        let buf = render_chart(
            DonutChart::new(vec![
                DonutSegment::new(3, Color::Green),
                DonutSegment::new(1, Color::Red),
            ]),
            20,
            10,
        );
        let mut ring_cells = 0;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let symbol = buf[(x, y)].symbol();
                if symbol == " " {
                    continue;
                }
                let glyph = symbol.chars().next().unwrap();
                assert!(
                    ('\u{2800}'..='\u{28ff}').contains(&glyph),
                    "expected a braille pattern at ({x}, {y}), got {symbol:?}"
                );
                ring_cells += 1;
            }
        }
        assert!(ring_cells > 0, "expected some ring cells");
    }

    #[test]
    fn ring_is_visually_round_on_a_wide_area() {
        let buf = render_chart(
            DonutChart::new(vec![DonutSegment::new(10, Color::Green)]),
            40,
            12,
        );
        let mut min_x = u16::MAX;
        let mut max_x = 0;
        let mut min_y = u16::MAX;
        let mut max_y = 0;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].symbol() != " " {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        let horizontal_span = max_x - min_x + 1;
        let vertical_span = max_y - min_y + 1;
        // Cells are ~1:2 and the ring is a true circle in square-dot space, so
        // its cell bounding box spans ~2x as many columns as rows.
        let ratio = f64::from(horizontal_span) / f64::from(vertical_span);
        assert!(
            (1.7..=2.3).contains(&ratio),
            "expected span ratio ≈ 2, got {horizontal_span}x{vertical_span} (ratio {ratio})"
        );
    }

    #[test]
    fn max_radius_caps_the_ring_and_keeps_it_centered() {
        let (width, height) = (60, 30);
        let buf = render_chart(
            DonutChart::new(vec![DonutSegment::new(10, Color::Green)]).max_radius(3.0),
            width,
            height,
        );
        let rows = ring_rows(&buf);
        assert!(
            (6..=7).contains(&rows.len()),
            "expected a vertical diameter of ≈ 6-7 rows, got {}",
            rows.len()
        );
        // The capped ring stays centered in the area.
        let top = *rows.first().unwrap();
        let bottom = *rows.last().unwrap();
        assert_eq!(top + bottom + 1, height, "ring is vertically centered");
        let middle_row = ring_cells_in_row(&buf, height / 2);
        let left = *middle_row.first().unwrap();
        let right = *middle_row.last().unwrap();
        assert_eq!(left + right + 1, width, "ring is horizontally centered");
    }

    #[test]
    fn boundary_cells_carry_one_of_the_two_segment_colors() {
        let buf = render_chart(
            DonutChart::new(vec![
                DonutSegment::new(1, Color::Green),
                DonutSegment::new(1, Color::Red),
            ]),
            20,
            10,
        );
        let mut green = 0;
        let mut red = 0;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].symbol() == " " {
                    continue;
                }
                match buf[(x, y)].fg {
                    Color::Green => green += 1,
                    Color::Red => red += 1,
                    other => panic!("unexpected ring color {other:?} at ({x}, {y})"),
                }
            }
        }
        assert!(
            green > 0 && red > 0,
            "expected both segment colors on the ring, got green={green} red={red}"
        );
    }

    #[test]
    fn center_text_lands_in_the_middle_of_the_area() {
        let buf = render_chart(
            DonutChart::new(vec![DonutSegment::new(10, Color::Green)])
                .center(vec![Line::from("38.2B"), Line::from("total")])
                .background(Color::Black),
            20,
            10,
        );
        // Bounding box is 5x2, so it starts at (7, 4) on a cleared background.
        assert_eq!(buf[(7, 4)].symbol(), "3");
        assert_eq!(buf[(8, 4)].symbol(), "8");
        assert_eq!(buf[(11, 4)].symbol(), "B");
        assert_eq!(buf[(7, 5)].symbol(), "t");
        assert_eq!(buf[(11, 5)].symbol(), "l");
        assert_eq!(buf[(7, 4)].bg, Color::Black);
        assert_eq!(buf[(11, 5)].bg, Color::Black);
    }

    #[test]
    fn tiny_and_empty_areas_do_not_panic() {
        for (width, height) in [(0, 0), (1, 0), (0, 1), (1, 1), (2, 1), (3, 2)] {
            let mut terminal = Terminal::new(TestBackend::new(8, 8)).unwrap();
            terminal
                .draw(|frame| {
                    let chart = DonutChart::new(vec![DonutSegment::new(10, Color::Green)])
                        .center(vec![Line::from("38.2B")]);
                    frame.render_widget(chart, Rect::new(1, 1, width, height));
                })
                .unwrap();
        }
    }
}
