use ratatui::style::Color;

pub(crate) const WCAG_AA_TEXT_CONTRAST: f64 = 4.5;
pub(crate) const WCAG_NON_TEXT_CONTRAST: f64 = 3.0;

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
/// matching the semantic theme contract. The target `foreground` must itself
/// satisfy `minimum_ratio`; an unreachable request is a theme contract error.
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
        panic!("semantic contrast requires RGB colors");
    };
    let background = Color::Rgb(background_red, background_green, background_blue);
    let foreground = Color::Rgb(foreground_red, foreground_green, foreground_blue);
    assert!(
        contrast_ratio(foreground, background) >= minimum_ratio,
        "contrast target cannot reach requested minimum ratio"
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_and_white_have_expected_relative_luminance() {
        assert_eq!(relative_luminance(Color::Rgb(0, 0, 0)), 0.0);
        assert_eq!(relative_luminance(Color::Rgb(255, 255, 255)), 1.0);
    }

    #[test]
    fn ensure_contrast_preserves_color_that_already_meets_ratio() {
        let color = Color::Rgb(220, 220, 220);

        assert_eq!(
            ensure_contrast(
                color,
                Color::Rgb(0, 0, 0),
                Color::Rgb(255, 255, 255),
                WCAG_AA_TEXT_CONTRAST,
            ),
            color
        );
    }

    #[test]
    fn ensure_contrast_blends_until_the_ratio_is_met() {
        let background = Color::Rgb(0, 0, 0);
        let source = Color::Rgb(80, 80, 80);
        let adjusted = ensure_contrast(
            source,
            background,
            Color::Rgb(255, 255, 255),
            WCAG_AA_TEXT_CONTRAST,
        );

        assert_ne!(adjusted, source);
        assert!(contrast_ratio(adjusted, background) >= WCAG_AA_TEXT_CONTRAST);
    }

    #[test]
    #[should_panic(expected = "contrast target cannot reach requested minimum ratio")]
    fn ensure_contrast_rejects_an_unreachable_ratio() {
        ensure_contrast(
            Color::Rgb(32, 32, 32),
            Color::Rgb(0, 0, 0),
            Color::Rgb(64, 64, 64),
            WCAG_AA_TEXT_CONTRAST,
        );
    }
}
