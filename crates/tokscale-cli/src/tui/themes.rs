use ratatui::style::{Color, Modifier, Style};

use super::contrast::contrast_ratio;

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
        let idx = themes.iter().position(|&theme| theme == self).unwrap_or(0);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfacePalette {
    pub(crate) canvas: Color,
    pub(crate) panel: Color,
    pub(crate) row_alt: Color,
    pub(crate) row_current: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextPalette {
    pub(crate) primary: Color,
    pub(crate) muted: Color,
    pub(crate) disabled: Color,
    pub(crate) inverse: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChromePalette {
    pub(crate) nav_active: Color,
    pub(crate) heading: Color,
    pub(crate) border: Color,
    pub(crate) focus: Color,
    pub(crate) current: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionPalette {
    pub(crate) background: Color,
    pub(crate) foreground: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MetricsPalette {
    pub(crate) tokens: Color,
    pub(crate) cost: Color,
    pub(crate) input: Color,
    pub(crate) output: Color,
    pub(crate) cache_read: Color,
    pub(crate) cache_write: Color,
    pub(crate) rate: Color,
    pub(crate) total: Color,
    pub(crate) secondary_cost: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusPalette {
    pub(crate) success: Color,
    pub(crate) warning: Color,
    pub(crate) danger: Color,
    pub(crate) info: Color,
    pub(crate) pending: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisualizationPalette {
    pub(crate) activity: [Color; 5],
    pub(crate) grid: Color,
    pub(crate) chart_highlight: Color,
    pub(crate) artwork: Color,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: ThemeName,
    pub(crate) surface: SurfacePalette,
    pub(crate) text: TextPalette,
    pub(crate) chrome: ChromePalette,
    pub(crate) selection: SelectionPalette,
    pub(crate) metrics: MetricsPalette,
    pub(crate) status: StatusPalette,
    pub(crate) visualization: VisualizationPalette,
}

impl Theme {
    pub fn from_name(name: ThemeName) -> Self {
        theme_for_name(name)
    }

    /// Chooses the semantic text color with the greatest contrast on `background`.
    pub(crate) fn contrasting_foreground(&self, background: Color) -> Color {
        let primary_contrast = contrast_ratio(self.text.primary, background);
        let inverse_contrast = contrast_ratio(self.text.inverse, background);
        if primary_contrast >= inverse_contrast {
            self.text.primary
        } else {
            self.text.inverse
        }
    }

    pub(crate) fn metric_input_style(&self) -> Style {
        Style::default().fg(self.metrics.input)
    }

    pub(crate) fn metric_output_style(&self) -> Style {
        Style::default().fg(self.metrics.output)
    }

    pub(crate) fn metric_cache_read_style(&self) -> Style {
        Style::default().fg(self.metrics.cache_read)
    }

    pub(crate) fn metric_cache_write_style(&self) -> Style {
        Style::default().fg(self.metrics.cache_write)
    }

    pub(crate) fn metric_total_style(&self) -> Style {
        Style::default()
            .fg(self.metrics.total)
            .add_modifier(Modifier::BOLD)
    }

    pub(crate) fn canvas_style(&self) -> Style {
        Style::default()
            .fg(self.text.primary)
            .bg(self.surface.canvas)
    }

    pub(crate) fn panel_style(&self) -> Style {
        Style::default()
            .fg(self.text.primary)
            .bg(self.surface.panel)
    }

    pub(crate) fn selection_style(&self) -> Style {
        Style::default()
            .fg(self.selection.foreground)
            .bg(self.selection.background)
            .add_modifier(Modifier::BOLD)
    }

    pub(crate) fn subtle_text_style(&self) -> Style {
        Style::default().fg(self.text.disabled)
    }

    pub(crate) fn striped_row_style(&self) -> Style {
        Style::default().bg(self.surface.row_alt)
    }

    pub(crate) fn current_row_style(&self) -> Style {
        Style::default().bg(self.surface.row_current)
    }
}

fn theme_for_name(name: ThemeName) -> Theme {
    match name {
        ThemeName::Green => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(7, 15, 10),
                panel: rgb(12, 24, 17),
                row_alt: rgb(16, 32, 22),
                row_current: rgb(22, 57, 35),
            },
            text: TextPalette {
                primary: rgb(226, 244, 232),
                muted: rgb(165, 195, 174),
                disabled: rgb(105, 132, 113),
                inverse: rgb(5, 18, 9),
            },
            chrome: ChromePalette {
                nav_active: rgb(126, 231, 135),
                heading: rgb(155, 233, 168),
                border: rgb(50, 82, 60),
                focus: rgb(86, 211, 100),
                current: rgb(110, 231, 125),
            },
            selection: SelectionPalette {
                background: rgb(24, 65, 36),
                foreground: rgb(240, 255, 244),
            },
            metrics: MetricsPalette {
                tokens: rgb(126, 231, 135),
                cost: rgb(255, 213, 102),
                input: rgb(115, 222, 147),
                output: rgb(255, 145, 145),
                cache_read: rgb(128, 196, 255),
                cache_write: rgb(255, 187, 120),
                rate: rgb(196, 181, 253),
                total: rgb(226, 244, 232),
                secondary_cost: rgb(207, 180, 110),
            },
            status: StatusPalette {
                success: rgb(126, 231, 135),
                warning: rgb(255, 213, 102),
                danger: rgb(255, 128, 128),
                info: rgb(128, 196, 255),
                pending: rgb(191, 201, 194),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(14, 29, 19),
                    rgb(33, 110, 57),
                    rgb(48, 161, 78),
                    rgb(64, 196, 99),
                    rgb(155, 233, 168),
                ],
                grid: rgb(40, 68, 49),
                chart_highlight: rgb(155, 233, 168),
                artwork: rgb(90, 205, 111),
            },
        },
        ThemeName::Halloween => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(20, 10, 3),
                panel: rgb(31, 17, 7),
                row_alt: rgb(42, 23, 8),
                row_current: rgb(72, 39, 8),
            },
            text: TextPalette {
                primary: rgb(255, 239, 211),
                muted: rgb(218, 181, 135),
                disabled: rgb(145, 111, 76),
                inverse: rgb(27, 12, 2),
            },
            chrome: ChromePalette {
                nav_active: rgb(255, 177, 66),
                heading: rgb(255, 210, 108),
                border: rgb(104, 62, 26),
                focus: rgb(255, 145, 28),
                current: rgb(255, 197, 66),
            },
            selection: SelectionPalette {
                background: rgb(88, 40, 5),
                foreground: rgb(255, 246, 225),
            },
            metrics: MetricsPalette {
                tokens: rgb(255, 192, 82),
                cost: rgb(255, 225, 105),
                input: rgb(151, 218, 112),
                output: rgb(255, 132, 95),
                cache_read: rgb(120, 190, 255),
                cache_write: rgb(255, 175, 78),
                rate: rgb(230, 167, 255),
                total: rgb(255, 239, 211),
                secondary_cost: rgb(224, 187, 119),
            },
            status: StatusPalette {
                success: rgb(151, 218, 112),
                warning: rgb(255, 210, 82),
                danger: rgb(255, 120, 91),
                info: rgb(120, 190, 255),
                pending: rgb(218, 181, 135),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(38, 18, 4),
                    rgb(99, 29, 0),
                    rgb(254, 150, 0),
                    rgb(255, 197, 1),
                    rgb(255, 238, 74),
                ],
                grid: rgb(83, 45, 16),
                chart_highlight: rgb(255, 210, 82),
                artwork: rgb(255, 126, 35),
            },
        },
        ThemeName::Teal => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(4, 17, 18),
                panel: rgb(7, 27, 29),
                row_alt: rgb(9, 37, 39),
                row_current: rgb(10, 61, 62),
            },
            text: TextPalette {
                primary: rgb(218, 246, 245),
                muted: rgb(151, 198, 196),
                disabled: rgb(91, 134, 133),
                inverse: rgb(2, 21, 22),
            },
            chrome: ChromePalette {
                nav_active: rgb(83, 224, 217),
                heading: rgb(126, 229, 229),
                border: rgb(34, 90, 91),
                focus: rgb(45, 197, 197),
                current: rgb(91, 218, 213),
            },
            selection: SelectionPalette {
                background: rgb(9, 63, 64),
                foreground: rgb(230, 255, 254),
            },
            metrics: MetricsPalette {
                tokens: rgb(83, 224, 217),
                cost: rgb(245, 216, 112),
                input: rgb(103, 219, 151),
                output: rgb(255, 137, 146),
                cache_read: rgb(116, 195, 255),
                cache_write: rgb(255, 181, 105),
                rate: rgb(202, 174, 255),
                total: rgb(218, 246, 245),
                secondary_cost: rgb(192, 190, 127),
            },
            status: StatusPalette {
                success: rgb(103, 219, 151),
                warning: rgb(245, 216, 112),
                danger: rgb(255, 125, 135),
                info: rgb(116, 195, 255),
                pending: rgb(174, 202, 201),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(7, 34, 35),
                    rgb(14, 109, 109),
                    rgb(13, 158, 158),
                    rgb(45, 197, 197),
                    rgb(126, 229, 229),
                ],
                grid: rgb(28, 72, 73),
                chart_highlight: rgb(126, 229, 229),
                artwork: rgb(47, 205, 196),
            },
        },
        ThemeName::Blue => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(6, 13, 24),
                panel: rgb(10, 22, 38),
                row_alt: rgb(13, 30, 50),
                row_current: rgb(17, 48, 82),
            },
            text: TextPalette {
                primary: rgb(224, 239, 255),
                muted: rgb(155, 190, 225),
                disabled: rgb(91, 126, 162),
                inverse: rgb(4, 16, 31),
            },
            chrome: ChromePalette {
                nav_active: rgb(111, 181, 255),
                heading: rgb(146, 202, 255),
                border: rgb(43, 79, 118),
                focus: rgb(56, 139, 253),
                current: rgb(103, 175, 255),
            },
            selection: SelectionPalette {
                background: rgb(16, 57, 105),
                foreground: rgb(240, 249, 255),
            },
            metrics: MetricsPalette {
                tokens: rgb(111, 181, 255),
                cost: rgb(249, 218, 120),
                input: rgb(105, 220, 148),
                output: rgb(255, 137, 146),
                cache_read: rgb(130, 198, 255),
                cache_write: rgb(255, 181, 105),
                rate: rgb(192, 176, 255),
                total: rgb(224, 239, 255),
                secondary_cost: rgb(196, 190, 137),
            },
            status: StatusPalette {
                success: rgb(105, 220, 148),
                warning: rgb(249, 218, 120),
                danger: rgb(255, 125, 135),
                info: rgb(111, 181, 255),
                pending: rgb(168, 195, 222),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(10, 29, 53),
                    rgb(13, 65, 157),
                    rgb(31, 111, 235),
                    rgb(56, 139, 253),
                    rgb(121, 184, 255),
                ],
                grid: rgb(35, 66, 99),
                chart_highlight: rgb(146, 202, 255),
                artwork: rgb(74, 151, 255),
            },
        },
        ThemeName::Pink => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(22, 8, 17),
                panel: rgb(34, 13, 27),
                row_alt: rgb(45, 17, 36),
                row_current: rgb(72, 25, 54),
            },
            text: TextPalette {
                primary: rgb(255, 231, 244),
                muted: rgb(222, 171, 200),
                disabled: rgb(151, 105, 132),
                inverse: rgb(28, 7, 20),
            },
            chrome: ChromePalette {
                nav_active: rgb(248, 148, 202),
                heading: rgb(255, 181, 220),
                border: rgb(105, 46, 79),
                focus: rgb(232, 104, 173),
                current: rgb(242, 135, 195),
            },
            selection: SelectionPalette {
                background: rgb(101, 32, 70),
                foreground: rgb(255, 240, 248),
            },
            metrics: MetricsPalette {
                tokens: rgb(248, 148, 202),
                cost: rgb(255, 219, 121),
                input: rgb(111, 221, 151),
                output: rgb(255, 139, 157),
                cache_read: rgb(126, 196, 255),
                cache_write: rgb(255, 183, 110),
                rate: rgb(205, 175, 255),
                total: rgb(255, 231, 244),
                secondary_cost: rgb(211, 181, 139),
            },
            status: StatusPalette {
                success: rgb(111, 221, 151),
                warning: rgb(255, 219, 121),
                danger: rgb(255, 127, 145),
                info: rgb(126, 196, 255),
                pending: rgb(207, 177, 194),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(46, 17, 35),
                    rgb(153, 40, 110),
                    rgb(191, 75, 138),
                    rgb(217, 97, 160),
                    rgb(240, 181, 210),
                ],
                grid: rgb(82, 37, 63),
                chart_highlight: rgb(255, 181, 220),
                artwork: rgb(229, 103, 173),
            },
        },
        ThemeName::Purple => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(15, 9, 25),
                panel: rgb(24, 14, 39),
                row_alt: rgb(32, 19, 51),
                row_current: rgb(51, 29, 79),
            },
            text: TextPalette {
                primary: rgb(243, 232, 255),
                muted: rgb(195, 171, 222),
                disabled: rgb(126, 101, 153),
                inverse: rgb(18, 8, 31),
            },
            chrome: ChromePalette {
                nav_active: rgb(185, 142, 255),
                heading: rgb(211, 181, 255),
                border: rgb(79, 52, 110),
                focus: rgb(163, 113, 247),
                current: rgb(205, 174, 255),
            },
            selection: SelectionPalette {
                background: rgb(70, 39, 110),
                foreground: rgb(249, 241, 255),
            },
            metrics: MetricsPalette {
                tokens: rgb(185, 142, 255),
                cost: rgb(249, 218, 120),
                input: rgb(105, 220, 148),
                output: rgb(255, 137, 146),
                cache_read: rgb(126, 196, 255),
                cache_write: rgb(255, 181, 105),
                rate: rgb(214, 174, 255),
                total: rgb(243, 232, 255),
                secondary_cost: rgb(205, 185, 141),
            },
            status: StatusPalette {
                success: rgb(105, 220, 148),
                warning: rgb(249, 218, 120),
                danger: rgb(255, 125, 135),
                info: rgb(126, 196, 255),
                pending: rgb(190, 177, 205),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(32, 20, 53),
                    rgb(110, 64, 201),
                    rgb(137, 87, 229),
                    rgb(163, 113, 247),
                    rgb(205, 180, 255),
                ],
                grid: rgb(63, 42, 87),
                chart_highlight: rgb(211, 181, 255),
                artwork: rgb(168, 113, 247),
            },
        },
        ThemeName::Orange => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(21, 11, 5),
                panel: rgb(34, 19, 9),
                row_alt: rgb(45, 25, 11),
                row_current: rgb(73, 39, 13),
            },
            text: TextPalette {
                primary: rgb(255, 238, 219),
                muted: rgb(221, 181, 145),
                disabled: rgb(149, 112, 82),
                inverse: rgb(27, 12, 3),
            },
            chrome: ChromePalette {
                nav_active: rgb(255, 138, 91),
                heading: rgb(255, 198, 128),
                border: rgb(105, 61, 29),
                focus: rgb(255, 140, 0),
                current: rgb(255, 175, 85),
            },
            selection: SelectionPalette {
                background: rgb(84, 42, 6),
                foreground: rgb(255, 244, 230),
            },
            metrics: MetricsPalette {
                tokens: rgb(255, 175, 85),
                cost: rgb(255, 220, 117),
                input: rgb(119, 219, 142),
                output: rgb(255, 132, 112),
                cache_read: rgb(122, 193, 255),
                cache_write: rgb(255, 177, 91),
                rate: rgb(207, 171, 255),
                total: rgb(255, 238, 219),
                secondary_cost: rgb(216, 182, 127),
            },
            status: StatusPalette {
                success: rgb(119, 219, 142),
                warning: rgb(255, 220, 117),
                danger: rgb(255, 120, 101),
                info: rgb(122, 193, 255),
                pending: rgb(215, 181, 151),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(43, 23, 8),
                    rgb(204, 85, 0),
                    rgb(255, 140, 0),
                    rgb(255, 179, 71),
                    rgb(255, 214, 153),
                ],
                grid: rgb(84, 48, 21),
                chart_highlight: rgb(255, 198, 128),
                artwork: rgb(255, 138, 31),
            },
        },
        ThemeName::Monochrome => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(10, 10, 10),
                panel: rgb(20, 20, 20),
                row_alt: rgb(28, 28, 28),
                row_current: rgb(48, 48, 48),
            },
            text: TextPalette {
                primary: rgb(238, 238, 238),
                muted: rgb(184, 184, 184),
                disabled: rgb(119, 119, 119),
                inverse: rgb(12, 12, 12),
            },
            chrome: ChromePalette {
                nav_active: rgb(238, 238, 238),
                heading: rgb(214, 214, 214),
                border: rgb(76, 76, 76),
                focus: rgb(199, 199, 199),
                current: rgb(225, 225, 225),
            },
            selection: SelectionPalette {
                background: rgb(70, 70, 70),
                foreground: rgb(255, 255, 255),
            },
            metrics: MetricsPalette {
                tokens: rgb(220, 220, 220),
                cost: rgb(204, 204, 204),
                input: rgb(214, 214, 214),
                output: rgb(230, 230, 230),
                cache_read: rgb(194, 194, 194),
                cache_write: rgb(224, 224, 224),
                rate: rgb(201, 201, 201),
                total: rgb(245, 245, 245),
                secondary_cost: rgb(180, 180, 180),
            },
            status: StatusPalette {
                success: rgb(224, 224, 224),
                warning: rgb(207, 207, 207),
                danger: rgb(238, 238, 238),
                info: rgb(194, 194, 194),
                pending: rgb(180, 180, 180),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(22, 22, 22),
                    rgb(56, 56, 56),
                    rgb(82, 82, 82),
                    rgb(117, 117, 117),
                    rgb(158, 158, 158),
                ],
                grid: rgb(65, 65, 65),
                chart_highlight: rgb(225, 225, 225),
                artwork: rgb(170, 170, 170),
            },
        },
        ThemeName::YlGnBu => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(7, 15, 24),
                panel: rgb(11, 25, 36),
                row_alt: rgb(14, 34, 46),
                row_current: rgb(25, 56, 61),
            },
            text: TextPalette {
                primary: rgb(235, 246, 223),
                muted: rgb(181, 205, 176),
                disabled: rgb(111, 139, 121),
                inverse: rgb(7, 20, 24),
            },
            chrome: ChromePalette {
                nav_active: rgb(216, 239, 116),
                heading: rgb(184, 232, 161),
                border: rgb(46, 85, 92),
                focus: rgb(92, 205, 187),
                current: rgb(183, 225, 128),
            },
            selection: SelectionPalette {
                background: rgb(23, 62, 69),
                foreground: rgb(244, 252, 231),
            },
            metrics: MetricsPalette {
                tokens: rgb(161, 218, 180),
                cost: rgb(236, 229, 120),
                input: rgb(116, 220, 145),
                output: rgb(255, 139, 139),
                cache_read: rgb(112, 199, 255),
                cache_write: rgb(255, 188, 104),
                rate: rgb(199, 179, 255),
                total: rgb(235, 246, 223),
                secondary_cost: rgb(194, 199, 139),
            },
            status: StatusPalette {
                success: rgb(116, 220, 145),
                warning: rgb(236, 229, 120),
                danger: rgb(255, 127, 132),
                info: rgb(112, 199, 255),
                pending: rgb(183, 201, 176),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(15, 32, 47),
                    rgb(37, 52, 148),
                    rgb(44, 127, 184),
                    rgb(65, 182, 196),
                    rgb(161, 218, 180),
                ],
                grid: rgb(38, 73, 82),
                chart_highlight: rgb(216, 239, 116),
                artwork: rgb(80, 197, 187),
            },
        },
        ThemeName::Graphite => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(8, 10, 14),
                panel: rgb(15, 18, 24),
                row_alt: rgb(21, 25, 33),
                row_current: rgb(27, 43, 48),
            },
            text: TextPalette {
                primary: rgb(226, 232, 240),
                muted: rgb(166, 178, 194),
                disabled: rgb(103, 116, 133),
                inverse: rgb(8, 12, 17),
            },
            chrome: ChromePalette {
                nav_active: rgb(125, 211, 252),
                heading: rgb(186, 230, 253),
                border: rgb(55, 65, 81),
                focus: rgb(56, 189, 248),
                current: rgb(103, 201, 241),
            },
            selection: SelectionPalette {
                background: rgb(24, 61, 77),
                foreground: rgb(240, 249, 255),
            },
            metrics: MetricsPalette {
                tokens: rgb(125, 211, 252),
                cost: rgb(250, 213, 116),
                input: rgb(110, 220, 151),
                output: rgb(255, 137, 146),
                cache_read: rgb(125, 193, 255),
                cache_write: rgb(255, 183, 105),
                rate: rgb(196, 181, 253),
                total: rgb(226, 232, 240),
                secondary_cost: rgb(198, 188, 139),
            },
            status: StatusPalette {
                success: rgb(110, 220, 151),
                warning: rgb(250, 213, 116),
                danger: rgb(255, 125, 135),
                info: rgb(125, 211, 252),
                pending: rgb(166, 178, 194),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(24, 27, 34),
                    rgb(14, 116, 144),
                    rgb(148, 163, 184),
                    rgb(56, 189, 248),
                    rgb(125, 211, 252),
                ],
                grid: rgb(48, 58, 72),
                chart_highlight: rgb(186, 230, 253),
                artwork: rgb(56, 189, 248),
            },
        },
        ThemeName::Lagoon => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(4, 18, 21),
                panel: rgb(7, 28, 31),
                row_alt: rgb(9, 38, 42),
                row_current: rgb(16, 60, 53),
            },
            text: TextPalette {
                primary: rgb(216, 241, 238),
                muted: rgb(157, 198, 194),
                disabled: rgb(93, 134, 132),
                inverse: rgb(3, 20, 22),
            },
            chrome: ChromePalette {
                nav_active: rgb(104, 240, 186),
                heading: rgb(153, 246, 228),
                border: rgb(31, 83, 88),
                focus: rgb(45, 212, 191),
                current: rgb(107, 230, 209),
            },
            selection: SelectionPalette {
                background: rgb(11, 65, 61),
                foreground: rgb(232, 255, 251),
            },
            metrics: MetricsPalette {
                tokens: rgb(94, 234, 212),
                cost: rgb(248, 220, 119),
                input: rgb(104, 224, 145),
                output: rgb(255, 139, 147),
                cache_read: rgb(117, 198, 255),
                cache_write: rgb(255, 183, 104),
                rate: rgb(197, 181, 253),
                total: rgb(216, 241, 238),
                secondary_cost: rgb(194, 193, 139),
            },
            status: StatusPalette {
                success: rgb(104, 224, 145),
                warning: rgb(248, 220, 119),
                danger: rgb(255, 127, 136),
                info: rgb(117, 198, 255),
                pending: rgb(170, 201, 198),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(6, 32, 36),
                    rgb(15, 118, 110),
                    rgb(45, 212, 191),
                    rgb(94, 234, 212),
                    rgb(153, 246, 228),
                ],
                grid: rgb(29, 76, 79),
                chart_highlight: rgb(153, 246, 228),
                artwork: rgb(45, 212, 191),
            },
        },
        ThemeName::Dusk => Theme {
            name,
            surface: SurfacePalette {
                canvas: rgb(14, 12, 23),
                panel: rgb(23, 20, 34),
                row_alt: rgb(31, 27, 44),
                row_current: rgb(48, 42, 67),
            },
            text: TextPalette {
                primary: rgb(232, 226, 238),
                muted: rgb(186, 174, 203),
                disabled: rgb(118, 105, 137),
                inverse: rgb(17, 13, 27),
            },
            chrome: ChromePalette {
                nav_active: rgb(224, 180, 255),
                heading: rgb(221, 214, 254),
                border: rgb(63, 57, 82),
                focus: rgb(167, 139, 250),
                current: rgb(192, 171, 252),
            },
            selection: SelectionPalette {
                background: rgb(60, 50, 94),
                foreground: rgb(248, 244, 255),
            },
            metrics: MetricsPalette {
                tokens: rgb(196, 181, 253),
                cost: rgb(249, 218, 120),
                input: rgb(105, 220, 148),
                output: rgb(255, 137, 146),
                cache_read: rgb(126, 196, 255),
                cache_write: rgb(255, 181, 105),
                rate: rgb(218, 175, 255),
                total: rgb(232, 226, 238),
                secondary_cost: rgb(203, 188, 144),
            },
            status: StatusPalette {
                success: rgb(105, 220, 148),
                warning: rgb(249, 218, 120),
                danger: rgb(255, 125, 135),
                info: rgb(126, 196, 255),
                pending: rgb(186, 174, 203),
            },
            visualization: VisualizationPalette {
                activity: [
                    rgb(27, 24, 38),
                    rgb(109, 40, 217),
                    rgb(139, 92, 246),
                    rgb(167, 139, 250),
                    rgb(196, 181, 253),
                ],
                grid: rgb(55, 49, 72),
                chart_highlight: rgb(221, 214, 254),
                artwork: rgb(167, 139, 250),
            },
        },
    }
}

fn rgb(red: u8, green: u8, blue: u8) -> Color {
    Color::Rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::contrast::{relative_luminance, WCAG_AA_TEXT_CONTRAST};

    fn rgb_contrast_ratio(first: Color, second: Color) -> f64 {
        contrast_ratio(first, second)
    }

    fn rgb_relative_luminance(color: Color) -> f64 {
        relative_luminance(color)
    }

    #[test]
    fn theme_names_round_trip_through_settings_values() {
        for theme in ThemeName::all() {
            assert_eq!(theme.as_str().parse::<ThemeName>(), Ok(*theme));
        }
    }

    #[test]
    fn themes_have_perceptibly_distinct_active_navigation_colors() {
        const MIN_RGB_DISTANCE_SQUARED: i32 = 30 * 30;

        let distance_squared = |first: Color, second: Color| {
            let (
                Color::Rgb(first_red, first_green, first_blue),
                Color::Rgb(second_red, second_green, second_blue),
            ) = (first, second)
            else {
                unreachable!("theme signatures must use RGB colors");
            };
            let red = i32::from(first_red) - i32::from(second_red);
            let green = i32::from(first_green) - i32::from(second_green);
            let blue = i32::from(first_blue) - i32::from(second_blue);
            red * red + green * green + blue * blue
        };

        for (index, &name) in ThemeName::all().iter().enumerate() {
            let theme = Theme::from_name(name);
            for &other_name in &ThemeName::all()[index + 1..] {
                let other = Theme::from_name(other_name);
                let distance = distance_squared(theme.chrome.nav_active, other.chrome.nav_active);
                assert!(
                    distance >= MIN_RGB_DISTANCE_SQUARED,
                    "{name:?} and {other_name:?} active navigation colors are too similar: squared RGB distance {distance}"
                );
            }
        }
    }

    #[test]
    fn activity_grades_do_not_decrease_in_luminance() {
        for &name in ThemeName::all() {
            let theme = Theme::from_name(name);
            let activity = theme.visualization.activity;

            let empty = rgb_relative_luminance(activity[0]);
            let first_activity = rgb_relative_luminance(activity[1]);
            assert!(
                empty < first_activity,
                "{name:?} grade 1 luminance ({first_activity:.4}) must exceed empty grade 0 ({empty:.4})"
            );

            for grade in 1..4 {
                let lower = rgb_relative_luminance(activity[grade]);
                let higher = rgb_relative_luminance(activity[grade + 1]);
                assert!(
                    lower <= higher,
                    "{name:?} grade {grade} luminance ({lower:.4}) exceeds grade {} ({higher:.4})",
                    grade + 1
                );
            }

            assert!(
                rgb_relative_luminance(activity[1]) < rgb_relative_luminance(activity[4]),
                "{name:?} highest activity grade must be brighter than its lowest activity grade"
            );
        }
    }

    #[test]
    fn monochrome_adjacent_grades_remain_visibly_distinct() {
        const MIN_ADJACENT_CONTRAST: f64 = 1.4;

        let theme = Theme::from_name(ThemeName::Monochrome);
        for grade in 0..4 {
            let contrast = rgb_contrast_ratio(
                theme.visualization.activity[grade],
                theme.visualization.activity[grade + 1],
            );
            assert!(
                contrast >= MIN_ADJACENT_CONTRAST,
                "Monochrome grades {grade} and {} have contrast {contrast:.2}, below {MIN_ADJACENT_CONTRAST:.2}",
                grade + 1
            );
        }
    }

    #[test]
    fn semantic_text_roles_are_readable_on_theme_surfaces() {
        for &name in ThemeName::all() {
            let theme = Theme::from_name(name);
            let panel = theme.surface.panel;

            let panel_text = [
                ("primary", theme.text.primary),
                ("muted", theme.text.muted),
                ("selection foreground", theme.selection.foreground),
                ("active navigation", theme.chrome.nav_active),
                ("heading", theme.chrome.heading),
                ("focus", theme.chrome.focus),
                ("current chrome", theme.chrome.current),
                ("tokens", theme.metrics.tokens),
                ("cost", theme.metrics.cost),
                ("input", theme.metrics.input),
                ("output", theme.metrics.output),
                ("cache read", theme.metrics.cache_read),
                ("cache write", theme.metrics.cache_write),
                ("rate", theme.metrics.rate),
                ("total", theme.metrics.total),
                ("secondary cost", theme.metrics.secondary_cost),
                ("success", theme.status.success),
                ("warning", theme.status.warning),
                ("danger", theme.status.danger),
                ("info", theme.status.info),
                ("pending", theme.status.pending),
            ];

            for (role, color) in panel_text {
                let contrast = rgb_contrast_ratio(color, panel);
                assert!(
                    contrast >= WCAG_AA_TEXT_CONTRAST,
                    "{name:?} {role} contrast {contrast:.2} is below {WCAG_AA_TEXT_CONTRAST:.1}"
                );
            }

            // Table cells carry their own foreground styles, so Row selection cannot
            // rely solely on SelectionPalette::foreground to remain readable.
            let selected_cell_text = [
                ("primary", theme.text.primary),
                ("muted", theme.text.muted),
                ("current chrome", theme.chrome.current),
                ("tokens", theme.metrics.tokens),
                ("cost", theme.metrics.cost),
                ("input", theme.metrics.input),
                ("output", theme.metrics.output),
                ("cache read", theme.metrics.cache_read),
                ("cache write", theme.metrics.cache_write),
                ("rate", theme.metrics.rate),
                ("total", theme.metrics.total),
                ("secondary cost", theme.metrics.secondary_cost),
                ("success", theme.status.success),
                ("warning", theme.status.warning),
                ("danger", theme.status.danger),
                ("info", theme.status.info),
                ("pending", theme.status.pending),
            ];
            for (role, color) in selected_cell_text {
                let contrast = rgb_contrast_ratio(color, theme.selection.background);
                assert!(
                    contrast >= WCAG_AA_TEXT_CONTRAST,
                    "{name:?} selected {role} contrast {contrast:.2} is below {WCAG_AA_TEXT_CONTRAST:.1}"
                );
            }

            let selection_contrast =
                rgb_contrast_ratio(theme.selection.foreground, theme.selection.background);
            assert!(
                selection_contrast >= WCAG_AA_TEXT_CONTRAST,
                "{name:?} selection contrast {selection_contrast:.2} is below {WCAG_AA_TEXT_CONTRAST:.1}"
            );
        }
    }

    #[test]
    fn core_status_roles_are_distinct_within_each_theme() {
        for &name in ThemeName::all() {
            let theme = Theme::from_name(name);
            let core_statuses = [
                ("success", theme.status.success),
                ("warning", theme.status.warning),
                ("danger", theme.status.danger),
            ];

            for (index, (role, color)) in core_statuses.iter().enumerate() {
                for (other_role, other_color) in &core_statuses[index + 1..] {
                    assert_ne!(
                        color, other_color,
                        "{name:?} {role} and {other_role} status colors must be distinct"
                    );
                }
            }
        }
    }

    #[test]
    fn contribution_selection_uses_the_more_contrasting_text_color() {
        for &name in ThemeName::all() {
            let theme = Theme::from_name(name);
            for color in theme.visualization.activity {
                let selected = theme.contrasting_foreground(color);
                let alternative = if selected == theme.text.primary {
                    theme.text.inverse
                } else {
                    theme.text.primary
                };
                assert!(
                    rgb_contrast_ratio(selected, color) >= rgb_contrast_ratio(alternative, color),
                    "{name:?} selected {selected:?} instead of the more legible {alternative:?} on {color:?}"
                );
            }
        }
    }

    #[test]
    fn style_helpers_use_semantic_roles() {
        let theme = Theme::from_name(ThemeName::Blue);

        assert_eq!(theme.metric_input_style().fg, Some(theme.metrics.input));
        assert_eq!(theme.metric_output_style().fg, Some(theme.metrics.output));
        assert_eq!(
            theme.metric_cache_read_style().fg,
            Some(theme.metrics.cache_read)
        );
        assert_eq!(
            theme.metric_cache_write_style().fg,
            Some(theme.metrics.cache_write)
        );
        assert_eq!(theme.metric_total_style().fg, Some(theme.metrics.total));
        assert_eq!(theme.canvas_style().fg, Some(theme.text.primary));
        assert_eq!(theme.canvas_style().bg, Some(theme.surface.canvas));
        assert_eq!(theme.panel_style().fg, Some(theme.text.primary));
        assert_eq!(theme.panel_style().bg, Some(theme.surface.panel));
        assert_eq!(theme.selection_style().fg, Some(theme.selection.foreground));
        assert_eq!(theme.selection_style().bg, Some(theme.selection.background));
        assert!(theme
            .selection_style()
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(theme.subtle_text_style().fg, Some(theme.text.disabled));
        assert_eq!(theme.striped_row_style().bg, Some(theme.surface.row_alt));
        assert_eq!(
            theme.current_row_style().bg,
            Some(theme.surface.row_current)
        );
    }
}
