use ratatui::style::Color;

pub(crate) const WCAG_AA_TEXT_CONTRAST: f64 = 4.5;

/// Returns the WCAG contrast ratio for two concrete RGB colors.
pub(crate) fn contrast_ratio(first: Color, second: Color) -> f64 {
    let first = relative_luminance(first);
    let second = relative_luminance(second);
    let (lighter, darker) = if first >= second {
        (first, second)
    } else {
        (second, first)
    };
    (lighter + 0.05) / (darker + 0.05)
}

pub(crate) fn relative_luminance(color: Color) -> f64 {
    let Color::Rgb(red, green, blue) = color else {
        panic!("semantic theme contrast requires RGB colors");
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

/// Moves an RGB color toward `foreground` until it reaches the requested
/// contrast against `background`.
///
/// The 255 discrete blend steps make the result deterministic while retaining
/// as much of the source color as possible. All inputs are required to be RGB,
/// matching the semantic theme contract.
pub(crate) fn ensure_contrast(
    color: Color,
    background: Color,
    foreground: Color,
    minimum_ratio: f64,
) -> Color {
    let (
        Color::Rgb(red, green, blue),
        Color::Rgb(background_red, background_green, background_blue),
        Color::Rgb(foreground_red, foreground_green, foreground_blue),
    ) = (color, background, foreground)
    else {
        panic!("semantic identity contrast requires RGB colors");
    };
    let background = Color::Rgb(background_red, background_green, background_blue);
    if contrast_ratio(color, background) >= minimum_ratio {
        return color;
    }

    let mix = |source: u8, target: u8, step: u16| {
        let source_weight = 255 - step;
        ((u16::from(source) * source_weight + u16::from(target) * step + 127) / 255) as u8
    };
    for step in 1..=255 {
        let candidate = Color::Rgb(
            mix(red, foreground_red, step),
            mix(green, foreground_green, step),
            mix(blue, foreground_blue, step),
        );
        if contrast_ratio(candidate, background) >= minimum_ratio {
            return candidate;
        }
    }

    foreground
}
