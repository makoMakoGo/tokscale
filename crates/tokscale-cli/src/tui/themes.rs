use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalColorMode {
    FullColor,
    Compatible,
}

impl TerminalColorMode {
    pub(crate) fn from_env<I, K, V>(env: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut term = String::new();
        let mut term_program = String::new();
        let mut colorterm = String::new();
        let mut no_color = false;

        for (key, value) in env {
            let key = key.as_ref();
            let value = value.as_ref();
            match key {
                "TERM" => term = value.to_ascii_lowercase(),
                "TERM_PROGRAM" => term_program = value.to_ascii_lowercase(),
                "COLORTERM" => colorterm = value.to_ascii_lowercase(),
                "NO_COLOR" => no_color = true,
                _ => {}
            }
        }

        if no_color || term == "dumb" || term_program == "apple_terminal" {
            return Self::Compatible;
        }

        if matches!(colorterm.as_str(), "truecolor" | "24bit")
            || term.contains("truecolor")
            || term.contains("24bit")
        {
            return Self::FullColor;
        }

        Self::FullColor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Green,
    Halloween,
    Teal,
    Blue,
    Pink,
    Purple,
    Orange,
    Monochrome,
    YlGnBu,
    Graphite,
    Lagoon,
    Dusk,
}

impl ThemeName {
    pub fn all() -> &'static [ThemeName] {
        &[
            ThemeName::Green,
            ThemeName::Halloween,
            ThemeName::Teal,
            ThemeName::Blue,
            ThemeName::Pink,
            ThemeName::Purple,
            ThemeName::Orange,
            ThemeName::Monochrome,
            ThemeName::YlGnBu,
            ThemeName::Graphite,
            ThemeName::Lagoon,
            ThemeName::Dusk,
        ]
    }

    pub fn next(self) -> ThemeName {
        let themes = Self::all();
        let idx = themes.iter().position(|&t| t == self).unwrap_or(0);
        themes[(idx + 1) % themes.len()]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ThemeName::Green => "green",
            ThemeName::Halloween => "halloween",
            ThemeName::Teal => "teal",
            ThemeName::Blue => "blue",
            ThemeName::Pink => "pink",
            ThemeName::Purple => "purple",
            ThemeName::Orange => "orange",
            ThemeName::Monochrome => "monochrome",
            ThemeName::YlGnBu => "ylgnbu",
            ThemeName::Graphite => "graphite",
            ThemeName::Lagoon => "lagoon",
            ThemeName::Dusk => "dusk",
        }
    }
}

impl std::str::FromStr for ThemeName {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "green" => Ok(ThemeName::Green),
            "halloween" => Ok(ThemeName::Halloween),
            "teal" => Ok(ThemeName::Teal),
            "blue" => Ok(ThemeName::Blue),
            "pink" => Ok(ThemeName::Pink),
            "purple" => Ok(ThemeName::Purple),
            "orange" => Ok(ThemeName::Orange),
            "monochrome" => Ok(ThemeName::Monochrome),
            "ylgnbu" => Ok(ThemeName::YlGnBu),
            "graphite" => Ok(ThemeName::Graphite),
            "lagoon" => Ok(ThemeName::Lagoon),
            "dusk" => Ok(ThemeName::Dusk),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: ThemeName,
    pub colors: [Color; 5],
    pub background: Color,
    pub foreground: Color,
    pub border: Color,
    pub highlight: Color,
    pub muted: Color,
    pub accent: Color,
    pub selection: Color,
    striped_row: Color,
    current_row: Color,
    color_mode: TerminalColorMode,
}

impl Theme {
    pub fn from_name_for_current_terminal(name: ThemeName) -> Self {
        Self::from_name_with_color_mode(name, TerminalColorMode::from_env(std::env::vars()))
    }

    pub(crate) fn from_name_with_color_mode(
        name: ThemeName,
        color_mode: TerminalColorMode,
    ) -> Self {
        let colors = match name {
            // Contribution grades become brighter as activity increases on the dark TUI surface.
            ThemeName::Green => [
                Color::Rgb(22, 27, 34),    // grade0: empty
                Color::Rgb(33, 110, 57),   // grade1: #216e39
                Color::Rgb(48, 161, 78),   // grade2: #30a14e
                Color::Rgb(64, 196, 99),   // grade3: #40c463
                Color::Rgb(155, 233, 168), // grade4: #9be9a8
            ],
            ThemeName::Halloween => [
                Color::Rgb(22, 27, 34),   // grade0: empty
                Color::Rgb(99, 29, 0),    // grade1: #631D00
                Color::Rgb(254, 150, 0),  // grade2: #FE9600
                Color::Rgb(255, 197, 1),  // grade3: #FFC501
                Color::Rgb(255, 238, 74), // grade4: #FFEE4A
            ],
            ThemeName::Teal => [
                Color::Rgb(22, 27, 34),    // grade0: empty
                Color::Rgb(14, 109, 109),  // grade1: #0e6d6d
                Color::Rgb(13, 158, 158),  // grade2: #0d9e9e
                Color::Rgb(45, 197, 197),  // grade3: #2dc5c5
                Color::Rgb(126, 229, 229), // grade4: #7ee5e5
            ],
            ThemeName::Blue => [
                Color::Rgb(22, 27, 34),    // grade0: empty
                Color::Rgb(13, 65, 157),   // grade1: #0d419d
                Color::Rgb(31, 111, 235),  // grade2: #1f6feb
                Color::Rgb(56, 139, 253),  // grade3: #388bfd
                Color::Rgb(121, 184, 255), // grade4: #79b8ff
            ],
            ThemeName::Pink => [
                Color::Rgb(22, 27, 34),    // grade0: empty
                Color::Rgb(153, 40, 110),  // grade1: #99286e
                Color::Rgb(191, 75, 138),  // grade2: #bf4b8a
                Color::Rgb(217, 97, 160),  // grade3: #d961a0
                Color::Rgb(240, 181, 210), // grade4: #f0b5d2
            ],
            ThemeName::Purple => [
                Color::Rgb(22, 27, 34),    // grade0: empty
                Color::Rgb(110, 64, 201),  // grade1: #6e40c9
                Color::Rgb(137, 87, 229),  // grade2: #8957e5
                Color::Rgb(163, 113, 247), // grade3: #a371f7
                Color::Rgb(205, 180, 255), // grade4: #cdb4ff
            ],
            ThemeName::Orange => [
                Color::Rgb(22, 27, 34),    // grade0: empty
                Color::Rgb(204, 85, 0),    // grade1: #cc5500
                Color::Rgb(255, 140, 0),   // grade2: #ff8c00
                Color::Rgb(255, 179, 71),  // grade3: #ffb347
                Color::Rgb(255, 214, 153), // grade4: #ffd699
            ],
            ThemeName::Monochrome => [
                Color::Rgb(22, 27, 34),    // grade0: empty
                Color::Rgb(56, 56, 56),    // grade1: #383838
                Color::Rgb(82, 82, 82),    // grade2: #525252
                Color::Rgb(117, 117, 117), // grade3: #757575
                Color::Rgb(158, 158, 158), // grade4: #9e9e9e
            ],
            ThemeName::YlGnBu => [
                Color::Rgb(22, 27, 34),    // grade0: empty
                Color::Rgb(37, 52, 148),   // grade1: #253494
                Color::Rgb(44, 127, 184),  // grade2: #2c7fb8
                Color::Rgb(65, 182, 196),  // grade3: #41b6c4
                Color::Rgb(161, 218, 180), // grade4: #a1dab4
            ],
            ThemeName::Graphite => [
                Color::Rgb(24, 27, 34),
                Color::Rgb(14, 116, 144),
                Color::Rgb(148, 163, 184),
                Color::Rgb(56, 189, 248),
                Color::Rgb(125, 211, 252),
            ],
            ThemeName::Lagoon => [
                Color::Rgb(6, 32, 36),
                Color::Rgb(15, 118, 110),
                Color::Rgb(45, 212, 191),
                Color::Rgb(94, 234, 212),
                Color::Rgb(153, 246, 228),
            ],
            ThemeName::Dusk => [
                Color::Rgb(27, 24, 38),
                Color::Rgb(109, 40, 217),
                Color::Rgb(139, 92, 246),
                Color::Rgb(167, 139, 250),
                Color::Rgb(196, 181, 253),
            ],
        };

        let mut theme = Self {
            name,
            colors,
            background: Color::Rgb(13, 17, 23),
            foreground: Color::Rgb(201, 209, 217),
            border: Color::Rgb(48, 54, 61),
            highlight: colors[4],
            muted: Color::Rgb(139, 148, 158),
            accent: Color::Cyan,
            selection: Color::Rgb(48, 54, 61),
            striped_row: Color::Rgb(20, 24, 30),
            current_row: Color::Rgb(28, 42, 34),
            color_mode,
        };

        match name {
            ThemeName::Graphite => {
                theme.background = Color::Rgb(10, 12, 16);
                theme.foreground = Color::Rgb(226, 232, 240);
                theme.border = Color::Rgb(55, 65, 81);
                theme.muted = Color::Rgb(148, 163, 184);
                theme.accent = Color::Rgb(125, 211, 252);
                theme.selection = Color::Rgb(31, 41, 55);
                theme.striped_row = Color::Rgb(15, 18, 24);
                theme.current_row = Color::Rgb(24, 39, 38);
            }
            ThemeName::Lagoon => {
                theme.background = Color::Rgb(5, 20, 23);
                theme.foreground = Color::Rgb(216, 241, 238);
                theme.border = Color::Rgb(31, 83, 88);
                theme.muted = Color::Rgb(133, 177, 175);
                theme.accent = Color::Rgb(94, 234, 212);
                theme.selection = Color::Rgb(15, 54, 58);
                theme.striped_row = Color::Rgb(7, 26, 30);
                theme.current_row = Color::Rgb(18, 54, 42);
            }
            ThemeName::Dusk => {
                theme.background = Color::Rgb(17, 16, 26);
                theme.foreground = Color::Rgb(232, 226, 238);
                theme.border = Color::Rgb(63, 57, 82);
                theme.muted = Color::Rgb(166, 154, 184);
                theme.accent = Color::Rgb(196, 181, 253);
                theme.selection = Color::Rgb(43, 37, 58);
                theme.striped_row = Color::Rgb(22, 20, 32);
                theme.current_row = Color::Rgb(40, 45, 36);
            }
            _ => {}
        }

        if color_mode == TerminalColorMode::Compatible {
            theme.colors = [
                Color::Black,
                Color::DarkGray,
                Color::Gray,
                Color::Cyan,
                Color::White,
            ];
            theme.background = Color::Black;
            theme.foreground = Color::White;
            theme.border = Color::DarkGray;
            theme.highlight = Color::Cyan;
            theme.muted = Color::DarkGray;
            theme.accent = Color::Cyan;
            theme.selection = Color::DarkGray;
            theme.striped_row = Color::Black;
            theme.current_row = Color::DarkGray;
        }

        theme
    }

    pub(crate) fn color(&self, color: Color) -> Color {
        match (self.color_mode, color) {
            (TerminalColorMode::Compatible, Color::Rgb(r, g, b)) => compatible_rgb(r, g, b),
            _ => color,
        }
    }

    /// Chooses whichever theme surface color is more legible on `background`.
    pub(crate) fn contrasting_foreground(&self, background: Color) -> Color {
        match (background, self.background, self.foreground) {
            (Color::Rgb(..), Color::Rgb(..), Color::Rgb(..)) => {
                let foreground_contrast = rgb_contrast_ratio(self.foreground, background);
                let background_contrast = rgb_contrast_ratio(self.background, background);
                if foreground_contrast >= background_contrast {
                    self.foreground
                } else {
                    self.background
                }
            }
            (Color::White | Color::Gray | Color::Cyan, _, _) => Color::Black,
            _ => Color::White,
        }
    }

    pub(crate) fn metric_input_style(&self) -> Style {
        Style::default().fg(self.color(Color::Rgb(100, 200, 100)))
    }

    pub(crate) fn metric_output_style(&self) -> Style {
        Style::default().fg(self.color(Color::Rgb(200, 100, 100)))
    }

    pub(crate) fn metric_cache_read_style(&self) -> Style {
        Style::default().fg(self.color(Color::Rgb(100, 150, 200)))
    }

    pub(crate) fn metric_cache_write_style(&self) -> Style {
        Style::default().fg(self.color(Color::Rgb(200, 150, 100)))
    }

    pub(crate) fn metric_total_style(&self) -> Style {
        Style::default()
            .fg(self.foreground)
            .add_modifier(Modifier::BOLD)
    }

    pub(crate) fn subtle_text_style(&self) -> Style {
        Style::default().fg(self.color(Color::Rgb(102, 102, 102)))
    }

    pub(crate) fn striped_row_style(&self) -> Style {
        if self.color_mode == TerminalColorMode::Compatible {
            Style::default()
        } else {
            Style::default().bg(self.striped_row)
        }
    }

    pub(crate) fn current_row_style(&self) -> Style {
        if self.color_mode == TerminalColorMode::Compatible {
            Style::default().bg(self.selection)
        } else {
            Style::default().bg(self.current_row)
        }
    }
}

fn rgb_contrast_ratio(first: Color, second: Color) -> f64 {
    let first = rgb_relative_luminance(first);
    let second = rgb_relative_luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

fn rgb_relative_luminance(color: Color) -> f64 {
    let Color::Rgb(red, green, blue) = color else {
        unreachable!("RGB contrast requires RGB colors");
    };
    let linearize = |channel: u8| {
        let channel = f64::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linearize(red) + 0.7152 * linearize(green) + 0.0722 * linearize(blue)
}

fn compatible_rgb(r: u8, g: u8, b: u8) -> Color {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);

    if max < 64 {
        return Color::Black;
    }

    if max.saturating_sub(min) < 40 {
        return if max < 160 {
            Color::DarkGray
        } else {
            Color::Gray
        };
    }

    if r >= g && r >= b {
        if g >= 150 {
            Color::Yellow
        } else if b >= 150 {
            Color::Magenta
        } else {
            Color::Red
        }
    } else if g >= r && g >= b {
        if b >= 150 {
            Color::Cyan
        } else {
            Color::Green
        }
    } else if r >= 150 {
        Color::Magenta
    } else if g >= 150 {
        Color::Cyan
    } else {
        Color::Blue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn relative_luminance(color: Color) -> f64 {
        let Color::Rgb(r, g, b) = color else {
            panic!("relative luminance requires an RGB color, got {color:?}");
        };

        fn linearize(channel: u8) -> f64 {
            let channel = f64::from(channel) / 255.0;
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        }

        0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
    }

    fn contrast_ratio(first: Color, second: Color) -> f64 {
        let first = relative_luminance(first);
        let second = relative_luminance(second);
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    }

    #[test]
    fn apple_terminal_uses_compatible_color_mode() {
        let mode = TerminalColorMode::from_env(env(&[
            ("TERM_PROGRAM", "Apple_Terminal"),
            ("TERM", "xterm-256color"),
        ]));

        assert_eq!(mode, TerminalColorMode::Compatible);
    }

    #[test]
    fn vscode_truecolor_keeps_full_color_mode() {
        let mode = TerminalColorMode::from_env(env(&[
            ("TERM_PROGRAM", "vscode"),
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
        ]));

        assert_eq!(mode, TerminalColorMode::FullColor);
    }

    #[test]
    fn no_color_forces_compatible_color_mode() {
        let mode =
            TerminalColorMode::from_env(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]));

        assert_eq!(mode, TerminalColorMode::Compatible);
    }

    #[test]
    fn theme_names_round_trip_through_settings_values() {
        for theme in ThemeName::all() {
            assert_eq!(theme.as_str().parse::<ThemeName>(), Ok(*theme));
        }
    }

    #[test]
    fn full_color_activity_grades_do_not_decrease_in_luminance() {
        for &name in ThemeName::all() {
            let theme = Theme::from_name_with_color_mode(name, TerminalColorMode::FullColor);

            let empty = relative_luminance(theme.colors[0]);
            let first_activity = relative_luminance(theme.colors[1]);
            assert!(
                empty < first_activity,
                "{name:?} grade 1 luminance ({first_activity:.4}) must exceed empty grade 0 ({empty:.4})"
            );

            for grade in 1..4 {
                let lower = relative_luminance(theme.colors[grade]);
                let higher = relative_luminance(theme.colors[grade + 1]);
                assert!(
                    lower <= higher,
                    "{name:?} grade {grade} luminance ({lower:.4}) exceeds grade {} ({higher:.4})",
                    grade + 1
                );
            }

            assert!(
                relative_luminance(theme.colors[1]) < relative_luminance(theme.colors[4]),
                "{name:?} highest activity grade must be brighter than its lowest activity grade"
            );
        }
    }

    #[test]
    fn monochrome_adjacent_grades_remain_visibly_distinct() {
        const MIN_ADJACENT_CONTRAST: f64 = 1.4;

        let theme =
            Theme::from_name_with_color_mode(ThemeName::Monochrome, TerminalColorMode::FullColor);
        for grade in 0..4 {
            let contrast = contrast_ratio(theme.colors[grade], theme.colors[grade + 1]);
            assert!(
                contrast >= MIN_ADJACENT_CONTRAST,
                "Monochrome grades {grade} and {} have contrast {contrast:.2}, below {MIN_ADJACENT_CONTRAST:.2}",
                grade + 1
            );
        }
    }

    #[test]
    fn compatible_activity_grades_end_with_the_brightest_color() {
        let expected = [
            Color::Black,
            Color::DarkGray,
            Color::Gray,
            Color::Cyan,
            Color::White,
        ];

        for &name in ThemeName::all() {
            let theme = Theme::from_name_with_color_mode(name, TerminalColorMode::Compatible);
            assert_eq!(theme.colors, expected, "{name:?}");
        }
    }

    #[test]
    fn contribution_selection_uses_the_more_contrasting_surface_color() {
        for &name in ThemeName::all() {
            let theme = Theme::from_name_with_color_mode(name, TerminalColorMode::FullColor);
            for color in theme.colors {
                let selected = theme.contrasting_foreground(color);
                let alternative = if selected == theme.foreground {
                    theme.background
                } else {
                    theme.foreground
                };
                assert!(
                    contrast_ratio(selected, color) >= contrast_ratio(alternative, color),
                    "{name:?} selected {selected:?} instead of the more legible {alternative:?} on {color:?}"
                );
            }
        }

        for &name in ThemeName::all() {
            let theme = Theme::from_name_with_color_mode(name, TerminalColorMode::Compatible);
            let foregrounds = theme
                .colors
                .map(|color| theme.contrasting_foreground(color));
            assert_eq!(
                foregrounds,
                [
                    Color::White,
                    Color::White,
                    Color::Black,
                    Color::Black,
                    Color::Black,
                ],
                "{name:?}"
            );
        }
    }

    #[test]
    fn surface_themes_customize_background_and_row_colors() {
        let cases = [
            (
                ThemeName::Graphite,
                Color::Rgb(10, 12, 16),
                Color::Rgb(226, 232, 240),
                Color::Rgb(31, 41, 55),
                Color::Rgb(15, 18, 24),
                Color::Rgb(24, 39, 38),
            ),
            (
                ThemeName::Lagoon,
                Color::Rgb(5, 20, 23),
                Color::Rgb(216, 241, 238),
                Color::Rgb(15, 54, 58),
                Color::Rgb(7, 26, 30),
                Color::Rgb(18, 54, 42),
            ),
            (
                ThemeName::Dusk,
                Color::Rgb(17, 16, 26),
                Color::Rgb(232, 226, 238),
                Color::Rgb(43, 37, 58),
                Color::Rgb(22, 20, 32),
                Color::Rgb(40, 45, 36),
            ),
        ];

        for (name, background, foreground, selection, striped, current) in cases {
            let theme = Theme::from_name_with_color_mode(name, TerminalColorMode::FullColor);
            assert_eq!(theme.background, background);
            assert_eq!(theme.foreground, foreground);
            assert_eq!(theme.selection, selection);
            assert_eq!(theme.striped_row_style().bg, Some(striped));
            assert_eq!(theme.current_row_style().bg, Some(current));
        }
    }

    #[test]
    fn compatible_theme_preserves_name_and_avoids_rgb_palette() {
        let theme =
            Theme::from_name_with_color_mode(ThemeName::Green, TerminalColorMode::Compatible);

        assert_eq!(theme.name, ThemeName::Green);
        assert!(theme
            .colors
            .iter()
            .all(|color| !matches!(color, Color::Rgb(..))));
        assert!(!matches!(theme.background, Color::Rgb(..)));
        assert_ne!(theme.background, Color::Reset);
        assert!(!matches!(theme.foreground, Color::Rgb(..)));
        assert!(!matches!(theme.selection, Color::Rgb(..)));
    }

    #[test]
    fn full_color_theme_preserves_rgb_accent_styles() {
        let theme = Theme::from_name_with_color_mode(ThemeName::Blue, TerminalColorMode::FullColor);

        assert_eq!(
            theme.metric_input_style().fg,
            Some(Color::Rgb(100, 200, 100))
        );
        assert_eq!(theme.striped_row_style().bg, Some(Color::Rgb(20, 24, 30)));
    }

    #[test]
    fn compatible_theme_downgrades_rgb_accent_styles() {
        let theme =
            Theme::from_name_with_color_mode(ThemeName::Blue, TerminalColorMode::Compatible);

        let styles = [
            theme.metric_input_style(),
            theme.metric_output_style(),
            theme.metric_cache_read_style(),
            theme.metric_cache_write_style(),
            theme.metric_total_style(),
            theme.subtle_text_style(),
            theme.striped_row_style(),
            theme.current_row_style(),
        ];

        for style in styles {
            assert!(
                !matches!(style.fg, Some(Color::Rgb(..))),
                "compatible foreground should not use RGB: {:?}",
                style.fg
            );
            assert!(
                !matches!(style.bg, Some(Color::Rgb(..))),
                "compatible background should not use RGB: {:?}",
                style.bg
            );
        }
    }
}
