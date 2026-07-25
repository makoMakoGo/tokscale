use ab_glyph::{point, Font, FontArc, GlyphId, PxScale, ScaleFont};
use anyhow::{Context, Result};
use chrono::{Datelike, Duration, Local, NaiveDate};
use image::{imageops::FilterType, Rgba, RgbaImage};
use imageproc::drawing::draw_filled_circle_mut;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tokscale_core::{inferred_provider_from_model, ClientId, GroupBy, UsageQuery};

const SCALE: i32 = 2;
const IMAGE_WIDTH: i32 = 1200 * SCALE;
const IMAGE_HEIGHT: i32 = 1200 * SCALE;
const PADDING: i32 = 56 * SCALE;

const WRAPPED_FOOTER_IDENTITY: &str = "@juya-ai/tokscale";
const PROVIDER_LOGO_ANTHROPIC_URL: &str =
    "https://raw.githubusercontent.com/makoMakoGo/tokscale/personal/local-clients/.github/assets/client-claude.jpg";
const PROVIDER_LOGO_OPENAI_URL: &str =
    "https://raw.githubusercontent.com/makoMakoGo/tokscale/personal/local-clients/.github/assets/client-openai.jpg";
const PROVIDER_LOGO_GOOGLE_URL: &str =
    "https://raw.githubusercontent.com/makoMakoGo/tokscale/personal/local-clients/.github/assets/client-gemini.png";
const PROVIDER_LOGO_XAI_URL: &str =
    "https://raw.githubusercontent.com/makoMakoGo/tokscale/personal/local-clients/.github/assets/provider-xai.png";
const PROVIDER_LOGO_ZAI_URL: &str =
    "https://raw.githubusercontent.com/makoMakoGo/tokscale/personal/local-clients/.github/assets/provider-zai.png";
const PROVIDER_LOGO_ANTHROPIC_CACHE_FILE: &str = "provider-anthropic-fork-v2@2x.jpg";
const PROVIDER_LOGO_OPENAI_CACHE_FILE: &str = "provider-openai-fork-v2@2x.jpg";
const PROVIDER_LOGO_GOOGLE_CACHE_FILE: &str = "provider-google-fork-v2@2x.png";
const PROVIDER_LOGO_XAI_CACHE_FILE: &str = "provider-xai-fork-v1@2x.png";
const PROVIDER_LOGO_ZAI_CACHE_FILE: &str = "provider-zai-fork-v1@2x.png";
const FIGTREE_REGULAR_FILE: &str = "Figtree-Regular.ttf";
const FIGTREE_REGULAR_URL: &str =
    "https://fonts.gstatic.com/s/figtree/v9/_Xmz-HUzqDCFdgfMsYiV_F7wfS-Bs_d_QF5e.ttf";
const FIGTREE_BOLD_FILE: &str = "Figtree-Bold.ttf";
const FIGTREE_BOLD_URL: &str =
    "https://fonts.gstatic.com/s/figtree/v9/_Xmz-HUzqDCFdgfMsYiV_F7wfS-Bs_eYR15e.ttf";

const COLOR_BACKGROUND: Rgba<u8> = Rgba([0x10, 0x12, 0x1C, 0xFF]);
const COLOR_TEXT_PRIMARY: Rgba<u8> = Rgba([0xFF, 0xFF, 0xFF, 0xFF]);
const COLOR_TEXT_SECONDARY: Rgba<u8> = Rgba([0x88, 0x88, 0x88, 0xFF]);
const COLOR_GRADE0: Rgba<u8> = Rgba([0x14, 0x1A, 0x25, 0xFF]);
const COLOR_GRADE1: Rgba<u8> = Rgba([0x00, 0xB2, 0xFF, 0x44]);
const COLOR_GRADE2: Rgba<u8> = Rgba([0x00, 0xB2, 0xFF, 0x88]);
const COLOR_GRADE3: Rgba<u8> = Rgba([0x00, 0xB2, 0xFF, 0xCC]);
const COLOR_GRADE4: Rgba<u8> = Rgba([0x00, 0xB2, 0xFF, 0xFF]);

#[derive(Debug, Clone)]
pub struct WrappedOptions {
    pub output: Option<String>,
    pub year: Option<String>,
    pub home_dir: Option<PathBuf>,
    pub clients: Option<Vec<ClientId>>,
    pub short: bool,
}

#[derive(Debug, Clone)]
struct WrappedData {
    health: tokscale_core::input_health::HealthSummary,
    year: String,
    active_days: i32,
    total_tokens: i64,
    total_cost: f64,
    longest_streak: i32,
    top_models: Vec<WrappedRankedEntry>,
    top_clients: Vec<WrappedRankedEntry>,
    contributions: Vec<WrappedContribution>,
    total_messages: i32,
}

#[derive(Debug, Clone)]
struct WrappedRankedEntry {
    name: String,
    client_id: Option<String>,
    provider: Option<&'static str>,
    cost: f64,
    tokens: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderLogoAsset {
    url: &'static str,
    cache_file: &'static str,
}

#[derive(Debug, Clone)]
struct WrappedContribution {
    date: String,
    level: u8,
}

#[derive(Debug, Clone)]
struct FontSet {
    regular: FontArc,
    bold: FontArc,
}

#[derive(Debug, Clone)]
struct RenderOptions {
    short: bool,
}

pub async fn run(options: WrappedOptions) -> Result<String> {
    generate_wrapped(options).await
}

async fn generate_wrapped(options: WrappedOptions) -> Result<String> {
    let data = load_wrapped_data(&options).await?;
    crate::commands::shared::emit_health_summary(&data.health);

    let image = generate_wrapped_image(
        &data,
        &RenderOptions {
            short: options.short,
        },
    )
    .await?;

    let output = options
        .output
        .clone()
        .unwrap_or_else(|| format!("tokscale-{}-wrapped.png", data.year));
    let output_path = PathBuf::from(&output);
    let absolute = if output_path.is_absolute() {
        output_path
    } else {
        std::env::current_dir()?.join(output_path)
    };

    image
        .save_with_format(&absolute, image::ImageFormat::Png)
        .with_context(|| format!("Failed to save wrapped image to {}", absolute.display()))?;

    Ok(absolute.to_string_lossy().to_string())
}

fn wrapped_active_day_count(daily: &[tokscale_core::usage_views::DailyUsage]) -> usize {
    daily.iter().filter(|day| day.tokens.total() > 0).count()
}

async fn load_wrapped_data(options: &WrappedOptions) -> Result<WrappedData> {
    let year = options
        .year
        .clone()
        .unwrap_or_else(|| Local::now().year().to_string());
    let clients = options.clients.clone().unwrap_or_else(default_clients);

    let since = format!("{}-01-01", year);
    let until = format!("{}-12-31", year);

    let loader = crate::generation::GenerationLoader::with_filters(
        options.home_dir.clone(),
        Some(since),
        Some(until),
        Some(year.clone()),
    );
    let prepared = loader.prepare(&clients)?;
    let generation = loader.build(prepared).await?;
    let data = generation.project(&UsageQuery::full(generation.universe(), GroupBy::Model))?;
    let health = data.health.clone();

    let mut client_map: HashMap<String, WrappedRankedEntry> = HashMap::new();
    let mut total_messages = 0i32;

    for day in &data.daily {
        total_messages = total_messages.saturating_add(
            day.message_count
                .try_into()
                .expect("wrapped daily message count exceeds i32::MAX"),
        );
        for (client, client_usage) in &day.client_breakdown {
            let client_name = client_display_name(client.as_str())
                .unwrap_or(client.as_str())
                .to_string();
            let client_entry =
                client_map
                    .entry(client.to_string())
                    .or_insert_with(|| WrappedRankedEntry {
                        name: client_name,
                        client_id: Some(client.to_string()),
                        provider: None,
                        cost: 0.0,
                        tokens: 0,
                    });
            client_entry.cost += client_usage.cost;
            client_entry.tokens = client_entry
                .tokens
                .checked_add(
                    client_usage
                        .tokens
                        .total()
                        .try_into()
                        .expect("wrapped client token total exceeds i64::MAX"),
                )
                .expect("wrapped client token total exceeds i64::MAX");
        }
    }

    let mut top_models: Vec<WrappedRankedEntry> = data
        .models
        .iter()
        .map(|model| WrappedRankedEntry {
            name: format_model_name(&model.display_name),
            client_id: None,
            provider: get_provider_from_model(&model.model_id),
            cost: model.cost,
            tokens: model
                .tokens
                .total()
                .try_into()
                .expect("wrapped model token total exceeds i64::MAX"),
        })
        .collect();
    top_models.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(Ordering::Equal));
    top_models.truncate(3);

    let mut top_clients: Vec<WrappedRankedEntry> = client_map.into_values().collect();
    top_clients.sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(Ordering::Equal));
    top_clients.truncate(3);

    let max_cost = data.daily.iter().map(|day| day.cost).fold(1.0, f64::max);
    let contributions: Vec<WrappedContribution> = data
        .daily
        .iter()
        .map(|day| WrappedContribution {
            date: day.date.to_string(),
            level: calculate_intensity(day.cost, max_cost),
        })
        .collect();

    Ok(WrappedData {
        health,
        year,
        active_days: wrapped_active_day_count(&data.daily)
            .try_into()
            .expect("wrapped active day count exceeds i32::MAX"),
        total_tokens: data
            .total_tokens
            .try_into()
            .expect("wrapped token total exceeds i64::MAX"),
        total_cost: data.total_cost,
        longest_streak: data
            .longest_streak
            .try_into()
            .expect("wrapped longest streak exceeds i32::MAX"),
        top_models,
        top_clients,
        contributions,
        total_messages,
    })
}

#[cfg(test)]
fn accumulate_wrapped_model(
    model_map: &mut HashMap<String, WrappedRankedEntry>,
    model_id: &str,
    cost: f64,
    tokens: i64,
) {
    let model_entry = model_map
        .entry(model_id.to_string())
        .or_insert_with(|| WrappedRankedEntry {
            name: format_model_name(model_id),
            client_id: None,
            provider: get_provider_from_model(model_id),
            cost: 0.0,
            tokens: 0,
        });
    model_entry.cost += cost;
    model_entry.tokens = model_entry
        .tokens
        .checked_add(tokens)
        .expect("wrapped model token total exceeds i64::MAX");
}

async fn generate_wrapped_image(data: &WrappedData, options: &RenderOptions) -> Result<RgbaImage> {
    let client = reqwest::Client::new();
    let fonts = ensure_fonts_loaded(&client).await?;

    let mut canvas =
        RgbaImage::from_pixel(IMAGE_WIDTH as u32, IMAGE_HEIGHT as u32, COLOR_BACKGROUND);

    let left_width = (IMAGE_WIDTH as f32 * 0.45) as i32;
    let right_width = (IMAGE_WIDTH as f32 * 0.55) as i32;
    let right_x = left_width;

    let mut y_pos = PADDING + 24 * SCALE;

    let title_text = format!("My Wrapped {}", data.year);

    draw_text_mut_baseline(
        &mut canvas,
        &fonts.bold,
        (28 * SCALE) as f32,
        COLOR_TEXT_PRIMARY,
        PADDING,
        y_pos,
        &title_text,
    );
    y_pos += 60 * SCALE;

    draw_text_mut_baseline(
        &mut canvas,
        &fonts.regular,
        (20 * SCALE) as f32,
        COLOR_TEXT_SECONDARY,
        PADDING,
        y_pos,
        "Total Tokens",
    );
    y_pos += 64 * SCALE;

    let total_tokens_display = if options.short {
        format_tokens_short(data.total_tokens)
    } else {
        format_number_with_commas_i64(data.total_tokens)
    };
    draw_text_mut_baseline(
        &mut canvas,
        &fonts.bold,
        (56 * SCALE) as f32,
        COLOR_GRADE4,
        PADDING,
        y_pos,
        &total_tokens_display,
    );
    y_pos += 50 * SCALE + 40 * SCALE;

    let logo_size = 32 * SCALE;
    let logo_radius = 6 * SCALE;

    draw_text_mut_baseline(
        &mut canvas,
        &fonts.regular,
        (20 * SCALE) as f32,
        COLOR_TEXT_SECONDARY,
        PADDING,
        y_pos,
        "Top Models",
    );
    y_pos += 48 * SCALE;

    for (index, model) in data.top_models.iter().enumerate() {
        draw_text_mut_baseline(
            &mut canvas,
            &fonts.bold,
            (32 * SCALE) as f32,
            COLOR_TEXT_PRIMARY,
            PADDING,
            y_pos,
            &(index + 1).to_string(),
        );

        let mut text_x = PADDING + 40 * SCALE;

        if let Some(provider) = model.provider {
            if let Some(asset) = provider_logo_asset(provider) {
                if let Ok(path) = fetch_and_cache_image(&client, asset.url, asset.cache_file).await
                {
                    if let Ok(logo) = load_rgba_image(&path) {
                        let logo_y = y_pos - logo_size + 6 * SCALE;
                        let logo_x = PADDING + 40 * SCALE;

                        draw_image_rounded(
                            &mut canvas,
                            &logo,
                            logo_x,
                            logo_y,
                            logo_size,
                            logo_size,
                            logo_radius,
                        );
                        draw_rounded_border(
                            &mut canvas,
                            logo_x,
                            logo_y,
                            logo_size,
                            logo_size,
                            logo_radius,
                            SCALE,
                            COLOR_GRADE0,
                        );

                        text_x = logo_x + logo_size + 12 * SCALE;
                    }
                }
            }
        }

        draw_text_mut_baseline(
            &mut canvas,
            &fonts.regular,
            (32 * SCALE) as f32,
            COLOR_TEXT_PRIMARY,
            text_x,
            y_pos,
            &model.name,
        );
        y_pos += 50 * SCALE;
    }
    y_pos += 40 * SCALE;

    draw_text_mut_baseline(
        &mut canvas,
        &fonts.regular,
        (20 * SCALE) as f32,
        COLOR_TEXT_SECONDARY,
        PADDING,
        y_pos,
        "Top Clients",
    );
    y_pos += 48 * SCALE;

    for (index, client_entry) in data.top_clients.iter().enumerate() {
        draw_text_mut_baseline(
            &mut canvas,
            &fonts.bold,
            (32 * SCALE) as f32,
            COLOR_TEXT_PRIMARY,
            PADDING,
            y_pos,
            &(index + 1).to_string(),
        );

        if let Some(client_id) = client_entry.client_id.as_deref() {
            let logo_url = client_logo_url(client_id);
            let filename = format!(
                "client-{}@2x.png",
                client_id.to_lowercase().replace('/', "-")
            );

            if let Some(logo_url) = logo_url {
                if let Ok(path) = fetch_and_cache_image(&client, logo_url, &filename).await {
                    if let Ok(logo) = load_rgba_image(&path) {
                        let logo_x = PADDING + 40 * SCALE;
                        let logo_y = y_pos - logo_size + 6 * SCALE;

                        draw_image_rounded(
                            &mut canvas,
                            &logo,
                            logo_x,
                            logo_y,
                            logo_size,
                            logo_size,
                            logo_radius,
                        );
                        draw_rounded_border(
                            &mut canvas,
                            logo_x,
                            logo_y,
                            logo_size,
                            logo_size,
                            logo_radius,
                            SCALE,
                            COLOR_GRADE0,
                        );
                    }
                }
            }
        }

        draw_text_mut_baseline(
            &mut canvas,
            &fonts.regular,
            (32 * SCALE) as f32,
            COLOR_TEXT_PRIMARY,
            PADDING + 40 * SCALE + logo_size + 12 * SCALE,
            y_pos,
            &client_entry.name,
        );
        y_pos += 50 * SCALE;
    }

    y_pos += 40 * SCALE;

    let stats_start_y = y_pos;
    let stat_width = (left_width - PADDING * 2) / 2;

    draw_stat(
        &mut canvas,
        &fonts,
        PADDING,
        stats_start_y,
        "Messages",
        &format_number_with_commas_i64(data.total_messages as i64),
    );
    draw_stat(
        &mut canvas,
        &fonts,
        PADDING + stat_width,
        stats_start_y,
        "Active Days",
        &data.active_days.to_string(),
    );
    draw_stat(
        &mut canvas,
        &fonts,
        PADDING,
        stats_start_y + 100 * SCALE,
        "Cost",
        &format_cost(data.total_cost),
    );
    draw_stat(
        &mut canvas,
        &fonts,
        PADDING + stat_width,
        stats_start_y + 100 * SCALE,
        "Streak",
        &format!("{}d", data.longest_streak),
    );

    draw_contribution_graph(
        &mut canvas,
        data,
        right_x,
        PADDING,
        right_width - PADDING,
        IMAGE_HEIGHT - PADDING * 2,
    );

    let footer_bottom_y = IMAGE_HEIGHT - PADDING;
    draw_text_mut_baseline(
        &mut canvas,
        &fonts.bold,
        (24 * SCALE) as f32,
        COLOR_TEXT_PRIMARY,
        PADDING,
        footer_bottom_y - 32 * SCALE,
        "Tokscale",
    );
    draw_text_mut_baseline(
        &mut canvas,
        &fonts.regular,
        (18 * SCALE) as f32,
        COLOR_TEXT_SECONDARY,
        PADDING,
        footer_bottom_y,
        WRAPPED_FOOTER_IDENTITY,
    );

    Ok(canvas)
}

fn draw_contribution_graph(
    canvas: &mut RgbaImage,
    data: &WrappedData,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    let year = data
        .year
        .parse::<i32>()
        .unwrap_or_else(|_| Local::now().year());
    let Some(start_date) = NaiveDate::from_ymd_opt(year, 1, 1) else {
        return;
    };
    let Some(end_date) = NaiveDate::from_ymd_opt(year, 12, 31) else {
        return;
    };

    let contrib_map: HashMap<&str, u8> = data
        .contributions
        .iter()
        .map(|contribution| (contribution.date.as_str(), contribution.level))
        .collect();

    const DAYS_PER_ROW: i32 = 14;
    let total_days = (end_date - start_date).num_days() + 1;
    let total_rows = ((total_days + (DAYS_PER_ROW as i64) - 1) / DAYS_PER_ROW as i64) as i32;

    let cell_size = ((height as f32 / total_rows as f32).floor() as i32)
        .min((width as f32 / DAYS_PER_ROW as f32).floor() as i32)
        .max(1);
    let dot_radius = (((cell_size - 2 * SCALE) as f32) / 2.0).floor().max(1.0) as i32;

    let graph_width = DAYS_PER_ROW * cell_size;
    let _graph_height = total_rows * cell_size;
    let offset_x = x + (width - graph_width) / 2;
    let offset_y = y;

    let grade_colors = [
        COLOR_GRADE0,
        COLOR_GRADE1,
        COLOR_GRADE2,
        COLOR_GRADE3,
        COLOR_GRADE4,
    ];

    let mut current_date = start_date;
    let mut day_index = 0i32;

    while current_date <= end_date {
        let date_key = current_date.format("%Y-%m-%d").to_string();
        let level = *contrib_map.get(date_key.as_str()).unwrap_or(&0);

        let col = day_index % DAYS_PER_ROW;
        let row = day_index / DAYS_PER_ROW;

        let center_x = offset_x + col * cell_size + cell_size / 2;
        let center_y = offset_y + row * cell_size + cell_size / 2;

        draw_filled_circle_mut(
            canvas,
            (center_x, center_y),
            dot_radius,
            grade_colors[level as usize],
        );

        current_date += Duration::days(1);
        day_index += 1;
    }
}

fn draw_stat(canvas: &mut RgbaImage, fonts: &FontSet, x: i32, y: i32, label: &str, value: &str) {
    draw_text_mut_baseline(
        canvas,
        &fonts.regular,
        (18 * SCALE) as f32,
        COLOR_TEXT_SECONDARY,
        x,
        y,
        label,
    );
    draw_text_mut_baseline(
        canvas,
        &fonts.bold,
        (36 * SCALE) as f32,
        COLOR_TEXT_PRIMARY,
        x,
        y + 48 * SCALE,
        value,
    );
}

fn draw_text_mut_baseline(
    canvas: &mut RgbaImage,
    font: &FontArc,
    font_size: f32,
    color: Rgba<u8>,
    x: i32,
    baseline_y: i32,
    text: &str,
) {
    let scale = PxScale::from(font_size);
    let scaled_font = font.as_scaled(scale);

    let mut caret_x = x as f32;
    let baseline = baseline_y as f32;
    let mut prev_glyph: Option<GlyphId> = None;

    for ch in text.chars() {
        let glyph_id = scaled_font.glyph_id(ch);
        if let Some(prev) = prev_glyph {
            caret_x += scaled_font.kern(prev, glyph_id);
        }

        let glyph = glyph_id.with_scale_and_position(scale, point(caret_x, baseline));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                blend_pixel_with_coverage(canvas, px, py, color, coverage);
            });
        }

        caret_x += scaled_font.h_advance(glyph_id);
        prev_glyph = Some(glyph_id);
    }
}

fn draw_image_rounded(
    canvas: &mut RgbaImage,
    image: &RgbaImage,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
) {
    if width <= 0 || height <= 0 {
        return;
    }

    let resized =
        image::imageops::resize(image, width as u32, height as u32, FilterType::CatmullRom);
    for dy in 0..height {
        for dx in 0..width {
            let px = x + dx;
            let py = y + dy;
            if point_in_rounded_rect(
                px as f32 + 0.5,
                py as f32 + 0.5,
                x as f32,
                y as f32,
                width as f32,
                height as f32,
                radius as f32,
            ) {
                let src = *resized.get_pixel(dx as u32, dy as u32);
                blend_pixel(canvas, px, py, src);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_rounded_border(
    canvas: &mut RgbaImage,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    line_width: i32,
    color: Rgba<u8>,
) {
    if width <= 0 || height <= 0 || line_width <= 0 {
        return;
    }

    for py in y..(y + height) {
        for px in x..(x + width) {
            let cx = px as f32 + 0.5;
            let cy = py as f32 + 0.5;
            let outer = point_in_rounded_rect(
                cx,
                cy,
                x as f32,
                y as f32,
                width as f32,
                height as f32,
                radius as f32,
            );
            if !outer {
                continue;
            }

            let inner = point_in_rounded_rect(
                cx,
                cy,
                (x + line_width) as f32,
                (y + line_width) as f32,
                (width - 2 * line_width) as f32,
                (height - 2 * line_width) as f32,
                (radius - line_width).max(0) as f32,
            );

            if !inner {
                blend_pixel(canvas, px, py, color);
            }
        }
    }
}

fn point_in_rounded_rect(
    px: f32,
    py: f32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
) -> bool {
    if width <= 0.0 || height <= 0.0 {
        return false;
    }

    let r = radius.max(0.0).min(width / 2.0).min(height / 2.0);
    if r <= 0.0 {
        return px >= x && px < x + width && py >= y && py < y + height;
    }

    let nearest_x = px.clamp(x + r, x + width - r);
    let nearest_y = py.clamp(y + r, y + height - r);
    let dx = px - nearest_x;
    let dy = py - nearest_y;
    dx * dx + dy * dy <= r * r
}

fn blend_pixel_with_coverage(
    canvas: &mut RgbaImage,
    x: i32,
    y: i32,
    color: Rgba<u8>,
    coverage: f32,
) {
    let mut src = color;
    src.0[3] = ((src.0[3] as f32) * coverage.clamp(0.0, 1.0)).round() as u8;
    blend_pixel(canvas, x, y, src);
}

fn blend_pixel(canvas: &mut RgbaImage, x: i32, y: i32, src: Rgba<u8>) {
    if x < 0 || y < 0 {
        return;
    }

    let width = canvas.width() as i32;
    let height = canvas.height() as i32;
    if x >= width || y >= height {
        return;
    }

    let dst = canvas.get_pixel_mut(x as u32, y as u32);
    let src_alpha = src.0[3] as f32 / 255.0;
    if src_alpha <= 0.0 {
        return;
    }

    let dst_alpha = dst.0[3] as f32 / 255.0;
    let out_alpha = src_alpha + dst_alpha * (1.0 - src_alpha);

    if out_alpha <= 0.0 {
        *dst = Rgba([0, 0, 0, 0]);
        return;
    }

    for channel in 0..3 {
        let src_channel = src.0[channel] as f32 / 255.0;
        let dst_channel = dst.0[channel] as f32 / 255.0;
        let out_channel =
            (src_channel * src_alpha + dst_channel * dst_alpha * (1.0 - src_alpha)) / out_alpha;
        dst.0[channel] = (out_channel * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    dst.0[3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

async fn ensure_fonts_loaded(client: &reqwest::Client) -> Result<FontSet> {
    let cache_dir = get_font_cache_dir()?;
    ensure_cache_dir(&cache_dir)?;

    let regular_path = cache_dir.join(FIGTREE_REGULAR_FILE);
    let bold_path = cache_dir.join(FIGTREE_BOLD_FILE);

    if !regular_path.exists() {
        let _ = fetch_to_file(client, FIGTREE_REGULAR_URL, &regular_path).await;
    }
    if !bold_path.exists() {
        let _ = fetch_to_file(client, FIGTREE_BOLD_URL, &bold_path).await;
    }

    let regular_font = if regular_path.exists() {
        fs::read(&regular_path)
            .ok()
            .and_then(|bytes| FontArc::try_from_vec(bytes).ok())
    } else {
        None
    };
    let bold_font = if bold_path.exists() {
        fs::read(&bold_path)
            .ok()
            .and_then(|bytes| FontArc::try_from_vec(bytes).ok())
    } else {
        None
    };

    let (regular, bold) = match (regular_font, bold_font) {
        (Some(regular), Some(bold)) => (regular, bold),
        (Some(regular), None) => (regular.clone(), regular),
        (None, Some(bold)) => (bold.clone(), bold),
        (None, None) => {
            anyhow::bail!(
                "Failed to load Figtree fonts. Could not download or parse cached font files."
            )
        }
    };

    Ok(FontSet { regular, bold })
}

async fn fetch_and_cache_image(
    client: &reqwest::Client,
    url: &str,
    filename: &str,
) -> Result<PathBuf> {
    let cache_dir = get_image_cache_dir()?;
    ensure_cache_dir(&cache_dir)?;

    let cached_path = cache_dir.join(filename);
    if cached_path.exists() {
        return Ok(cached_path);
    }
    fetch_to_file(client, url, &cached_path).await?;

    Ok(cached_path)
}

async fn fetch_to_file(client: &reqwest::Client, url: &str, path: &Path) -> Result<()> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch {}", url))?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to fetch {} (status {})", url, response.status());
    }

    let bytes = response.bytes().await?;
    atomic_write_bytes(path, &bytes)?;
    Ok(())
}

fn ensure_cache_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn load_rgba_image(path: &Path) -> Result<RgbaImage> {
    Ok(image::open(path)
        .with_context(|| format!("Failed to decode image {}", path.display()))?
        .to_rgba8())
}

fn get_image_cache_dir() -> Result<PathBuf> {
    Ok(crate::paths::try_get_cache_dir()?.join("images"))
}

fn get_font_cache_dir() -> Result<PathBuf> {
    Ok(crate::paths::try_get_cache_dir()?.join("fonts"))
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    tokscale_core::fs_atomic::write_atomic(path, bytes)
        .with_context(|| format!("Failed to write cache file: {}", path.display()))
}

fn calculate_intensity(cost: f64, max_cost: f64) -> u8 {
    if cost == 0.0 || max_cost == 0.0 {
        return 0;
    }

    let ratio = cost / max_cost;
    if ratio >= 0.75 {
        4
    } else if ratio >= 0.5 {
        3
    } else if ratio >= 0.25 {
        2
    } else {
        1
    }
}

fn format_tokens_short(tokens: i64) -> String {
    if tokens >= 1_000_000_000 {
        format!("{:.2}B", tokens as f64 / 1_000_000_000.0)
    } else if tokens >= 1_000_000 {
        format!("{:.2}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn format_cost(cost: f64) -> String {
    if cost >= 1000.0 {
        format!("${:.2}K", cost / 1000.0)
    } else {
        format!("${:.2}", cost)
    }
}

fn format_number_with_commas_i64(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let digits = value.abs().to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3 + sign.len());

    result.push_str(sign);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }

    result
}

fn client_display_name(client: &str) -> Option<&'static str> {
    ClientId::from_str(client).map(ClientId::display_name)
}

fn client_logo_url(client: &str) -> Option<&'static str> {
    ClientId::from_str(client).map(|id| id.identity().logo_url)
}

fn provider_logo_asset(provider: &str) -> Option<ProviderLogoAsset> {
    match provider {
        "anthropic" => Some(ProviderLogoAsset {
            url: PROVIDER_LOGO_ANTHROPIC_URL,
            cache_file: PROVIDER_LOGO_ANTHROPIC_CACHE_FILE,
        }),
        "openai" => Some(ProviderLogoAsset {
            url: PROVIDER_LOGO_OPENAI_URL,
            cache_file: PROVIDER_LOGO_OPENAI_CACHE_FILE,
        }),
        "google" => Some(ProviderLogoAsset {
            url: PROVIDER_LOGO_GOOGLE_URL,
            cache_file: PROVIDER_LOGO_GOOGLE_CACHE_FILE,
        }),
        "xai" => Some(ProviderLogoAsset {
            url: PROVIDER_LOGO_XAI_URL,
            cache_file: PROVIDER_LOGO_XAI_CACHE_FILE,
        }),
        "zai" => Some(ProviderLogoAsset {
            url: PROVIDER_LOGO_ZAI_URL,
            cache_file: PROVIDER_LOGO_ZAI_CACHE_FILE,
        }),
        _ => None,
    }
}

fn get_provider_from_model(model_id: &str) -> Option<&'static str> {
    inferred_provider_from_model(model_id)
        .filter(|provider| provider_logo_asset(provider).is_some())
}

fn format_model_name(model: &str) -> String {
    if let Some(display) = exact_model_display_name(model) {
        return display.to_string();
    }

    let (without_quality, suffix) = split_quality_suffix(model);
    let mut cleaned = without_quality;

    if let Some(index) = cleaned.rfind(':') {
        let tail = &cleaned[index + 1..];
        if !tail.is_empty()
            && tail
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            cleaned.truncate(index);
        }
    }

    if cleaned.to_lowercase().ends_with("-thinking") {
        cleaned.truncate(cleaned.len() - "-thinking".len());
    } else if cleaned.to_lowercase().ends_with("_thinking") {
        cleaned.truncate(cleaned.len() - "_thinking".len());
    }

    cleaned = strip_date_suffix(cleaned);

    if let Some(display) = format_gpt_5_series_model_name(&cleaned, &suffix) {
        return display;
    }

    let normalized = cleaned
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>();

    if normalized.contains("claudeopus45") {
        return format!("Claude Opus 4.5{}", suffix);
    }
    if normalized.contains("claude4opus") {
        return format!("Claude 4 Opus{}", suffix);
    }
    if normalized.contains("claudeopus4") {
        return format!("Claude Opus 4{}", suffix);
    }
    if normalized.contains("claudesonnet45") {
        return format!("Claude Sonnet 4.5{}", suffix);
    }
    if normalized.contains("claude4sonnet") {
        return format!("Claude 4 Sonnet{}", suffix);
    }
    if normalized.contains("claudesonnet4") {
        return format!("Claude Sonnet 4{}", suffix);
    }
    if normalized.contains("claudehaiku45") {
        return format!("Claude Haiku 4.5{}", suffix);
    }
    if normalized.contains("claude4haiku") {
        return format!("Claude 4 Haiku{}", suffix);
    }
    if normalized.contains("claudehaiku4") {
        return format!("Claude Haiku 4{}", suffix);
    }
    if normalized.contains("claude37sonnet") {
        return format!("Claude 3.7 Sonnet{}", suffix);
    }
    if normalized.contains("claude35sonnet") {
        return format!("Claude 3.5 Sonnet{}", suffix);
    }
    if normalized.contains("claude35haiku") {
        return format!("Claude 3.5 Haiku{}", suffix);
    }
    if normalized.contains("claude3opus") {
        return format!("Claude 3 Opus{}", suffix);
    }
    if normalized.contains("claude3sonnet") {
        return format!("Claude 3 Sonnet{}", suffix);
    }
    if normalized.contains("claude3haiku") {
        return format!("Claude 3 Haiku{}", suffix);
    }
    if normalized.contains("gpt51") {
        return format!("GPT-5.1{}", suffix);
    }
    if normalized.contains("gpt5") {
        return format!("GPT-5{}", suffix);
    }
    if normalized.contains("gpt4omini") {
        return format!("GPT-4o Mini{}", suffix);
    }
    if normalized.contains("gpt4o") {
        return format!("GPT-4o{}", suffix);
    }
    if normalized.contains("gpt4turbo") {
        return format!("GPT-4 Turbo{}", suffix);
    }
    if normalized.contains("gpt4") {
        return format!("GPT-4{}", suffix);
    }
    if normalized.starts_with("o1mini") {
        return format!("o1 Mini{}", suffix);
    }
    if normalized.starts_with("o1preview") {
        return format!("o1 Preview{}", suffix);
    }
    if normalized.starts_with("o3mini") {
        return format!("o3 Mini{}", suffix);
    }
    if normalized == "o1" {
        return format!("o1{}", suffix);
    }
    if normalized == "o3" {
        return format!("o3{}", suffix);
    }
    if normalized.contains("gemini3pro") {
        return format!("Gemini 3 Pro{}", suffix);
    }
    if normalized.contains("gemini3flash") {
        return format!("Gemini 3 Flash{}", suffix);
    }
    if normalized.contains("gemini25pro") {
        return format!("Gemini 2.5 Pro{}", suffix);
    }
    if normalized.contains("gemini25flash") {
        return format!("Gemini 2.5 Flash{}", suffix);
    }
    if normalized.contains("gemini20flash") {
        return format!("Gemini 2.0 Flash{}", suffix);
    }
    if normalized.contains("gemini15pro") {
        return format!("Gemini 1.5 Pro{}", suffix);
    }
    if normalized.contains("gemini15flash") {
        return format!("Gemini 1.5 Flash{}", suffix);
    }
    if normalized.contains("grok3mini") {
        return format!("Grok Code 3 Mini{}", suffix);
    }
    if normalized.contains("grok3") {
        return format!("Grok Code 3{}", suffix);
    }
    if normalized.contains("grok") {
        return format!("Grok Code{}", suffix);
    }
    if normalized.contains("deepseekv3") {
        return format!("DeepSeek V3{}", suffix);
    }
    if normalized.contains("deepseekr1") {
        return format!("DeepSeek R1{}", suffix);
    }
    if normalized.contains("deepseek") {
        return format!("DeepSeek{}", suffix);
    }

    let mut fallback = cleaned;
    let fallback_lower = fallback.to_lowercase();
    if fallback_lower.starts_with("claude-") || fallback_lower.starts_with("claude_") {
        fallback = format!("Claude {}", &fallback[7..]);
    } else if fallback_lower.starts_with("gpt-") || fallback_lower.starts_with("gpt_") {
        fallback = format!("GPT-{}", &fallback[4..]);
    } else if fallback_lower.starts_with("gemini-") || fallback_lower.starts_with("gemini_") {
        fallback = format!("Gemini {}", &fallback[7..]);
    } else if fallback_lower.starts_with("grok-") || fallback_lower.starts_with("grok_") {
        fallback = format!("Grok Code {}", &fallback[5..]);
    }

    let base_name = fallback
        .split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(capitalize_word)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    if base_name.is_empty() {
        format!("{}{}", model, suffix)
    } else {
        format!("{}{}", base_name, suffix)
    }
}

fn format_gpt_5_series_model_name(model: &str, suffix: &str) -> Option<String> {
    let lower = model.to_lowercase();
    let model_part = lower
        .rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(&lower);

    let mut parts = model_part.split(['-', '_']).filter(|part| !part.is_empty());

    if parts.next()? != "gpt" {
        return None;
    }

    let version = parts.next()?;
    if version != "5" && !version.starts_with("5.") {
        return None;
    }

    let mut display = format!("GPT-{}", version);
    for part in parts {
        if part.starts_with("20") && part.chars().all(|ch| ch.is_ascii_digit()) {
            break;
        }

        match part {
            "pro" => display.push_str(" Pro"),
            "mini" => display.push_str(" Mini"),
            "nano" => display.push_str(" Nano"),
            "codex" => display.push_str(" Codex"),
            "max" => display.push_str(" Max"),
            "spark" => display.push_str(" Spark"),
            "sol" => display.push_str(" Sol"),
            "terra" => display.push_str(" Terra"),
            "luna" => display.push_str(" Luna"),
            "chat" => display.push_str(" Chat"),
            "preview" => display.push_str(" Preview"),
            "latest" => display.push_str(" Latest"),
            _ if part.chars().all(|ch| ch.is_ascii_digit()) => break,
            _ => {}
        }
    }

    display.push_str(suffix);
    Some(display)
}

fn exact_model_display_name(model: &str) -> Option<&'static str> {
    match model {
        "claude-sonnet-4-20250514" => Some("Claude Sonnet 4"),
        "claude-3-5-sonnet-20241022" => Some("Claude 3.5 Sonnet"),
        "claude-3-5-sonnet-20240620" => Some("Claude 3.5 Sonnet"),
        "claude-3-opus-20240229" => Some("Claude 3 Opus"),
        "claude-3-haiku-20240307" => Some("Claude 3 Haiku"),
        "gpt-4o" => Some("GPT-4o"),
        "gpt-4o-mini" => Some("GPT-4o Mini"),
        "gpt-4-turbo" => Some("GPT-4 Turbo"),
        "o1" => Some("o1"),
        "o1-mini" => Some("o1 Mini"),
        "o1-preview" => Some("o1 Preview"),
        "o3-mini" => Some("o3 Mini"),
        "gemini-2.5-pro" => Some("Gemini 2.5 Pro"),
        "gemini-2.5-flash" => Some("Gemini 2.5 Flash"),
        "gemini-2.0-flash" => Some("Gemini 2.0 Flash"),
        "gemini-1.5-pro" => Some("Gemini 1.5 Pro"),
        "gemini-1.5-flash" => Some("Gemini 1.5 Flash"),
        "grok-3" => Some("Grok 3"),
        "grok-3-mini" => Some("Grok 3 Mini"),
        _ => None,
    }
}

fn split_quality_suffix(model: &str) -> (String, String) {
    let lower = model.to_lowercase();

    for (needle, label) in [
        ("-xhigh", " XHigh"),
        ("_xhigh", " XHigh"),
        ("-high", " High"),
        ("_high", " High"),
        ("-medium", " Medium"),
        ("_medium", " Medium"),
        ("-low", " Low"),
        ("_low", " Low"),
    ] {
        if lower.ends_with(needle) {
            let base = model[..model.len() - needle.len()].to_string();
            return (base, label.to_string());
        }
    }

    (model.to_string(), String::new())
}

fn strip_date_suffix(mut model: String) -> String {
    let lower = model.to_lowercase();
    if lower.len() > 9 {
        if let Some(last_dash) = model.rfind('-') {
            let tail = &model[last_dash + 1..];
            if tail.len() == 8 && tail.chars().all(|ch| ch.is_ascii_digit()) {
                model.truncate(last_dash);
            }
        }
    }

    if let Some(last_dash) = model.rfind('-') {
        let tail = &model[last_dash + 1..];
        if tail.chars().all(|ch| ch.is_ascii_digit()) {
            if let Some(prev_dash) = model[..last_dash].rfind('-') {
                let prev_tail = &model[prev_dash + 1..last_dash];
                if prev_tail.starts_with("20")
                    && prev_tail.len() >= 8
                    && prev_tail.len() <= 10
                    && prev_tail.chars().all(|ch| ch.is_ascii_digit())
                {
                    model.truncate(prev_dash);
                }
            }
        }
    }

    model
}

fn capitalize_word(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }

    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut result = String::new();
    result.extend(first.to_uppercase());
    result.push_str(&chars.as_str().to_lowercase());
    result
}

fn default_clients() -> Vec<ClientId> {
    ClientId::iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tokscale_core::usage_views::{DailyUsage, UsageTokenBreakdown};

    #[test]
    fn wrapped_active_days_exclude_zero_usage_buckets() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 23).unwrap();
        let empty = DailyUsage {
            date,
            tokens: UsageTokenBreakdown::default(),
            cost: 0.0,
            client_breakdown: BTreeMap::new(),
            message_count: 0,
            turn_count: 0,
        };
        let active = DailyUsage {
            date: date.succ_opt().unwrap(),
            tokens: UsageTokenBreakdown {
                reasoning: 1,
                ..Default::default()
            },
            cost: 0.0,
            client_breakdown: BTreeMap::new(),
            message_count: 1,
            turn_count: 1,
        };

        assert_eq!(wrapped_active_day_count(&[empty, active]), 1);
    }

    // ========== format_tokens_short tests ==========

    #[test]
    fn test_format_tokens_short_billions() {
        assert_eq!(format_tokens_short(1_500_000_000), "1.50B");
        assert_eq!(format_tokens_short(2_340_000_000), "2.34B");
        assert_eq!(format_tokens_short(10_000_000_000), "10.00B");
    }

    #[test]
    fn test_format_tokens_short_millions() {
        assert_eq!(format_tokens_short(234_000_000), "234.00M");
        assert_eq!(format_tokens_short(1_500_000), "1.50M");
        assert_eq!(format_tokens_short(999_999_999), "1000.00M");
    }

    #[test]
    fn test_format_tokens_short_thousands() {
        assert_eq!(format_tokens_short(5_678), "5.7K");
        assert_eq!(format_tokens_short(1_000), "1.0K");
        assert_eq!(format_tokens_short(999_999), "1000.0K");
    }

    #[test]
    fn test_format_tokens_short_small_numbers() {
        assert_eq!(format_tokens_short(123), "123");
        assert_eq!(format_tokens_short(0), "0");
        assert_eq!(format_tokens_short(999), "999");
    }

    // ========== format_cost tests ==========

    #[test]
    fn test_format_cost_standard() {
        assert_eq!(format_cost(12.34), "$12.34");
        assert_eq!(format_cost(0.99), "$0.99");
        assert_eq!(format_cost(100.00), "$100.00");
    }

    #[test]
    fn test_format_cost_thousands() {
        assert_eq!(format_cost(1234.56), "$1.23K");
        assert_eq!(format_cost(5000.00), "$5.00K");
        assert_eq!(format_cost(999.99), "$999.99");
    }

    #[test]
    fn test_format_cost_zero() {
        assert_eq!(format_cost(0.0), "$0.00");
    }

    // ========== format_number_with_commas_i64 tests ==========

    #[test]
    fn test_format_number_with_commas_i64_large() {
        assert_eq!(format_number_with_commas_i64(1_234_567), "1,234,567");
        assert_eq!(
            format_number_with_commas_i64(1_000_000_000),
            "1,000,000,000"
        );
    }

    #[test]
    fn test_format_number_with_commas_i64_small() {
        assert_eq!(format_number_with_commas_i64(100), "100");
        assert_eq!(format_number_with_commas_i64(999), "999");
    }

    #[test]
    fn test_format_number_with_commas_i64_negative() {
        assert_eq!(format_number_with_commas_i64(-1_234_567), "-1,234,567");
        assert_eq!(format_number_with_commas_i64(-100), "-100");
    }

    #[test]
    fn test_format_number_with_commas_i64_zero() {
        assert_eq!(format_number_with_commas_i64(0), "0");
    }

    // ========== capitalize_word tests ==========

    #[test]
    fn test_capitalize_word_lowercase() {
        assert_eq!(capitalize_word("hello"), "Hello");
        assert_eq!(capitalize_word("world"), "World");
    }

    #[test]
    fn test_capitalize_word_uppercase() {
        assert_eq!(capitalize_word("WORLD"), "World");
        assert_eq!(capitalize_word("HELLO"), "Hello");
    }

    #[test]
    fn test_capitalize_word_mixed() {
        assert_eq!(capitalize_word("hElLo"), "Hello");
        assert_eq!(capitalize_word("WoRlD"), "World");
    }

    #[test]
    fn test_capitalize_word_empty() {
        assert_eq!(capitalize_word(""), "");
    }

    #[test]
    fn test_capitalize_word_single_char() {
        assert_eq!(capitalize_word("a"), "A");
        assert_eq!(capitalize_word("Z"), "Z");
    }

    // ========== strip_date_suffix tests ==========

    #[test]
    fn test_strip_date_suffix_with_date() {
        assert_eq!(
            strip_date_suffix("claude-4-20250514".to_string()),
            "claude-4"
        );
        assert_eq!(strip_date_suffix("gpt-4-20240101".to_string()), "gpt-4");
    }

    #[test]
    fn test_strip_date_suffix_without_date() {
        assert_eq!(strip_date_suffix("gpt-5".to_string()), "gpt-5");
        assert_eq!(strip_date_suffix("claude-opus".to_string()), "claude-opus");
    }

    #[test]
    fn test_strip_date_suffix_complex() {
        assert_eq!(
            strip_date_suffix("model-2024-01-15-123".to_string()),
            "model-2024-01-15-123"
        );
    }

    // ========== split_quality_suffix tests ==========

    #[test]
    fn test_split_quality_suffix_high() {
        assert_eq!(
            split_quality_suffix("model-high"),
            ("model".to_string(), " High".to_string())
        );
        assert_eq!(
            split_quality_suffix("gpt-4_high"),
            ("gpt-4".to_string(), " High".to_string())
        );
    }

    #[test]
    fn test_split_quality_suffix_xhigh() {
        assert_eq!(
            split_quality_suffix("model-xhigh"),
            ("model".to_string(), " XHigh".to_string())
        );
    }

    #[test]
    fn test_split_quality_suffix_medium() {
        assert_eq!(
            split_quality_suffix("model-medium"),
            ("model".to_string(), " Medium".to_string())
        );
    }

    #[test]
    fn test_split_quality_suffix_low() {
        assert_eq!(
            split_quality_suffix("model-low"),
            ("model".to_string(), " Low".to_string())
        );
    }

    #[test]
    fn test_split_quality_suffix_none() {
        assert_eq!(
            split_quality_suffix("model"),
            ("model".to_string(), String::new())
        );
        assert_eq!(
            split_quality_suffix("gpt-4"),
            ("gpt-4".to_string(), String::new())
        );
    }

    // ========== format_model_name tests ==========

    #[test]
    fn test_wrapped_keeps_gpt_5_6_family_models_distinct() {
        let mut model_map = HashMap::new();
        for model_id in ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            accumulate_wrapped_model(&mut model_map, model_id, 1.0, 1);
        }

        assert_eq!(model_map.len(), 3);
        let mut names = model_map
            .values()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["GPT-5.6 Luna", "GPT-5.6 Sol", "GPT-5.6 Terra"]);
    }

    #[test]
    fn test_wrapped_merges_gpt_5_6_alias_with_sol() {
        let mut model_map = HashMap::new();
        for raw_model_id in ["gpt-5.6", "gpt-5.6-sol"] {
            let model_id = tokscale_core::normalize_model_for_grouping(raw_model_id);
            accumulate_wrapped_model(&mut model_map, &model_id, 1.0, 1);
        }

        assert_eq!(model_map.len(), 1);
        let model = model_map.values().next().unwrap();
        assert_eq!(model.name, "GPT-5.6 Sol");
        assert_eq!(model.cost, 2.0);
        assert_eq!(model.tokens, 2);
    }

    #[test]
    fn test_format_model_name_claude() {
        assert_eq!(
            format_model_name("claude-sonnet-4-20250514"),
            "Claude Sonnet 4"
        );
        assert_eq!(
            format_model_name("claude-3-5-sonnet-20241022"),
            "Claude 3.5 Sonnet"
        );
        assert_eq!(format_model_name("claude-3-opus-20240229"), "Claude 3 Opus");
    }

    #[test]
    fn test_format_model_name_gpt() {
        assert_eq!(format_model_name("gpt-4o"), "GPT-4o");
        assert_eq!(format_model_name("gpt-4o-mini"), "GPT-4o Mini");
        assert_eq!(format_model_name("gpt-5"), "GPT-5");
        assert_eq!(format_model_name("gpt-5.5"), "GPT-5.5");
        assert_eq!(
            format_model_name("openai/gpt-5.5-pro-20260423"),
            "GPT-5.5 Pro"
        );
        assert_eq!(format_model_name("gpt-5.5-xhigh"), "GPT-5.5 XHigh");
    }

    #[test]
    fn test_format_model_name_gemini() {
        assert_eq!(format_model_name("gemini-2.5-pro"), "Gemini 2.5 Pro");
        assert_eq!(format_model_name("gemini-1.5-flash"), "Gemini 1.5 Flash");
    }

    #[test]
    fn test_format_model_name_with_quality_suffix() {
        assert_eq!(format_model_name("gpt-4-high"), "GPT-4 High");
        assert_eq!(format_model_name("claude-opus-low"), "Claude opus Low");
    }

    // ========== get_provider_from_model tests ==========

    #[test]
    fn test_get_provider_from_model_anthropic() {
        assert_eq!(get_provider_from_model("claude-3-opus"), Some("anthropic"));
        assert_eq!(get_provider_from_model("sonnet-4"), Some("anthropic"));
        assert_eq!(get_provider_from_model("haiku-3.5"), Some("anthropic"));
    }

    #[test]
    fn test_get_provider_from_model_openai() {
        assert_eq!(get_provider_from_model("gpt-4"), Some("openai"));
        assert_eq!(get_provider_from_model("o1-preview"), Some("openai"));
        assert_eq!(get_provider_from_model("codex-001"), Some("openai"));
    }

    #[test]
    fn test_get_provider_from_model_google() {
        assert_eq!(get_provider_from_model("gemini-pro"), Some("google"));
        assert_eq!(get_provider_from_model("gemini-2.5-flash"), Some("google"));
    }

    #[test]
    fn test_get_provider_from_model_xai() {
        assert_eq!(get_provider_from_model("grok-3"), Some("xai"));
        assert_eq!(get_provider_from_model("grok-code"), Some("xai"));
    }

    #[test]
    fn test_get_provider_from_model_zai() {
        assert_eq!(get_provider_from_model("glm-4.7"), Some("zai"));
        assert_eq!(get_provider_from_model("pickle-model"), None);
    }

    #[test]
    fn test_get_provider_from_model_unknown() {
        assert_eq!(get_provider_from_model("unknown-model"), None);
        assert_eq!(get_provider_from_model("random-123"), None);
    }

    #[test]
    fn test_wrapped_model_entry_keeps_provider_from_raw_model_id() {
        let raw_model_id = "claude-sonnet-4-20250514";
        let entry = WrappedRankedEntry {
            name: format_model_name(raw_model_id),
            client_id: None,
            provider: get_provider_from_model(raw_model_id),
            cost: 1.0,
            tokens: 1,
        };

        assert_eq!(entry.name, "Claude Sonnet 4");
        assert_eq!(entry.provider, Some("anthropic"));
        assert_eq!(get_provider_from_model(&entry.name), None);
    }

    // ========== calculate_intensity tests ==========

    #[test]
    fn test_calculate_intensity_grade4() {
        assert_eq!(calculate_intensity(100.0, 100.0), 4);
        assert_eq!(calculate_intensity(75.0, 100.0), 4);
        assert_eq!(calculate_intensity(80.0, 100.0), 4);
    }

    #[test]
    fn test_calculate_intensity_grade3() {
        assert_eq!(calculate_intensity(50.0, 100.0), 3);
        assert_eq!(calculate_intensity(60.0, 100.0), 3);
        assert_eq!(calculate_intensity(74.9, 100.0), 3);
    }

    #[test]
    fn test_calculate_intensity_grade2() {
        assert_eq!(calculate_intensity(25.0, 100.0), 2);
        assert_eq!(calculate_intensity(30.0, 100.0), 2);
        assert_eq!(calculate_intensity(49.9, 100.0), 2);
    }

    #[test]
    fn test_calculate_intensity_grade1() {
        assert_eq!(calculate_intensity(10.0, 100.0), 1);
        assert_eq!(calculate_intensity(24.9, 100.0), 1);
        assert_eq!(calculate_intensity(0.1, 100.0), 1);
    }

    #[test]
    fn test_calculate_intensity_grade0() {
        assert_eq!(calculate_intensity(0.0, 100.0), 0);
        assert_eq!(calculate_intensity(0.0, 0.0), 0);
    }

    // ========== client catalog tests ==========

    #[test]
    fn client_display_name_uses_catalog_for_every_client() {
        for client in ClientId::iter() {
            assert_eq!(
                client_display_name(client.as_str()),
                Some(client.display_name())
            );
        }
    }

    #[test]
    fn client_display_name_rejects_unknown_ids() {
        assert_eq!(client_display_name("unknown"), None);
        assert_eq!(client_display_name(""), None);
        assert_eq!(client_display_name("Claude"), None);
    }

    #[test]
    fn default_clients_use_the_complete_catalog() {
        let clients = default_clients();
        let expected = ClientId::iter().collect::<Vec<_>>();

        assert_eq!(clients, expected);
        assert!(clients.contains(&ClientId::Grok));
        assert!(clients.contains(&ClientId::Kiro));
        assert!(clients.contains(&ClientId::Warp));
    }

    #[test]
    fn client_logo_url_uses_catalog_for_every_client() {
        for client in ClientId::iter() {
            assert_eq!(
                client_logo_url(client.as_str()),
                Some(client.identity().logo_url)
            );
        }
    }

    #[test]
    fn client_logo_url_rejects_display_names_and_unknown_ids() {
        assert_eq!(client_logo_url("OpenCode"), None);
        assert_eq!(client_logo_url("Unknown"), None);
        assert_eq!(client_logo_url(""), None);
    }

    #[test]
    fn wrapped_footer_identity_uses_fork_package() {
        assert_eq!(WRAPPED_FOOTER_IDENTITY, "@juya-ai/tokscale");
    }

    // ========== provider_logo_asset tests ==========

    #[test]
    fn test_provider_logo_asset_anthropic() {
        assert_eq!(
            provider_logo_asset("anthropic"),
            Some(ProviderLogoAsset {
                url: PROVIDER_LOGO_ANTHROPIC_URL,
                cache_file: PROVIDER_LOGO_ANTHROPIC_CACHE_FILE,
            })
        );
    }

    #[test]
    fn test_provider_logo_asset_openai() {
        assert_eq!(
            provider_logo_asset("openai"),
            Some(ProviderLogoAsset {
                url: PROVIDER_LOGO_OPENAI_URL,
                cache_file: PROVIDER_LOGO_OPENAI_CACHE_FILE,
            })
        );
    }

    #[test]
    fn test_provider_logo_asset_google() {
        assert_eq!(
            provider_logo_asset("google"),
            Some(ProviderLogoAsset {
                url: PROVIDER_LOGO_GOOGLE_URL,
                cache_file: PROVIDER_LOGO_GOOGLE_CACHE_FILE,
            })
        );
    }

    #[test]
    fn test_provider_logo_asset_xai() {
        assert_eq!(
            provider_logo_asset("xai"),
            Some(ProviderLogoAsset {
                url: PROVIDER_LOGO_XAI_URL,
                cache_file: PROVIDER_LOGO_XAI_CACHE_FILE,
            })
        );
    }

    #[test]
    fn test_provider_logo_asset_zai() {
        assert_eq!(
            provider_logo_asset("zai"),
            Some(ProviderLogoAsset {
                url: PROVIDER_LOGO_ZAI_URL,
                cache_file: PROVIDER_LOGO_ZAI_CACHE_FILE,
            })
        );
    }

    #[test]
    fn test_provider_logo_asset_unknown() {
        assert_eq!(provider_logo_asset("unknown"), None);
        assert_eq!(provider_logo_asset(""), None);
        assert_eq!(provider_logo_asset("Anthropic"), None); // case-sensitive
    }

    // ========== capitalize_word edge case tests ==========

    #[test]
    fn test_capitalize_word_unicode() {
        // Unicode chars that have uppercase variants
        assert_eq!(capitalize_word("über"), "Über");
        assert_eq!(capitalize_word("état"), "État");
    }

    #[test]
    fn test_capitalize_word_numbers() {
        assert_eq!(capitalize_word("123abc"), "123abc");
        assert_eq!(capitalize_word("42"), "42");
    }

    #[test]
    fn test_capitalize_word_with_hyphens() {
        // capitalize_word only handles a single word, hyphens stay
        assert_eq!(capitalize_word("hello-world"), "Hello-world");
    }

    #[test]
    fn test_capitalize_word_all_lowercase_long() {
        assert_eq!(capitalize_word("abcdefghij"), "Abcdefghij");
    }

    // ========== format_number_with_commas_i64 edge case tests ==========

    #[test]
    fn test_format_number_with_commas_i64_single_digit() {
        assert_eq!(format_number_with_commas_i64(1), "1");
        assert_eq!(format_number_with_commas_i64(9), "9");
    }

    #[test]
    fn test_format_number_with_commas_i64_exact_thousands() {
        assert_eq!(format_number_with_commas_i64(1_000), "1,000");
        assert_eq!(format_number_with_commas_i64(1_000_000), "1,000,000");
    }

    #[test]
    fn test_format_number_with_commas_i64_large_negative() {
        assert_eq!(
            format_number_with_commas_i64(-1_000_000_000),
            "-1,000,000,000"
        );
    }

    #[test]
    fn test_format_number_with_commas_i64_max_value() {
        // i64::MAX = 9_223_372_036_854_775_807
        let result = format_number_with_commas_i64(i64::MAX);
        assert_eq!(result, "9,223,372,036,854,775,807");
    }

    #[test]
    fn test_format_number_with_commas_i64_large_negative_max() {
        let result = format_number_with_commas_i64(-9_223_372_036_854_775_807);
        assert_eq!(result, "-9,223,372,036,854,775,807");
    }

    #[test]
    fn test_format_number_with_commas_i64_boundary_999() {
        assert_eq!(format_number_with_commas_i64(999), "999");
        assert_eq!(format_number_with_commas_i64(1000), "1,000");
    }

    // ========== format_cost edge case tests ==========

    #[test]
    fn test_format_cost_very_large() {
        assert_eq!(format_cost(1_000_000.0), "$1000.00K");
        assert_eq!(format_cost(50_000.0), "$50.00K");
    }

    #[test]
    fn test_format_cost_very_small() {
        assert_eq!(format_cost(0.001), "$0.00");
        assert_eq!(format_cost(0.005), "$0.01"); // rounds up
        assert_eq!(format_cost(0.01), "$0.01");
    }

    #[test]
    fn test_format_cost_negative() {
        // Negative costs produce negative dollar string
        assert_eq!(format_cost(-5.50), "$-5.50");
    }

    #[test]
    fn test_format_cost_boundary_at_1000() {
        assert_eq!(format_cost(999.99), "$999.99");
        assert_eq!(format_cost(1000.0), "$1.00K");
        assert_eq!(format_cost(1000.01), "$1.00K");
    }

    #[test]
    fn test_format_cost_fractional_thousands() {
        assert_eq!(format_cost(1500.0), "$1.50K");
        assert_eq!(format_cost(2999.99), "$3.00K");
    }

    // ========== get_provider_from_model edge case tests ==========

    #[test]
    fn test_get_provider_from_model_case_insensitive() {
        assert_eq!(get_provider_from_model("CLAUDE-3-OPUS"), Some("anthropic"));
        assert_eq!(get_provider_from_model("GPT-4"), Some("openai"));
        assert_eq!(get_provider_from_model("Gemini-Pro"), Some("google"));
        assert_eq!(get_provider_from_model("GROK-3"), Some("xai"));
        assert_eq!(get_provider_from_model("GLM-4.7"), Some("zai"));
    }

    #[test]
    fn test_get_provider_from_model_partial_matches() {
        // "opus" alone triggers anthropic
        assert_eq!(get_provider_from_model("opus-4"), Some("anthropic"));
        // "o1" triggers openai
        assert_eq!(get_provider_from_model("o1-mini"), Some("openai"));
        // "o3" triggers openai
        assert_eq!(get_provider_from_model("o3-mini"), Some("openai"));
    }

    #[test]
    fn test_get_provider_from_model_model_with_provider_prefix() {
        assert_eq!(
            get_provider_from_model("anthropic/claude-sonnet-4"),
            Some("anthropic")
        );
        assert_eq!(get_provider_from_model("openai/gpt-4o"), Some("openai"));
    }

    #[test]
    fn test_get_provider_from_model_empty_string() {
        assert_eq!(get_provider_from_model(""), None);
    }

    #[test]
    fn test_get_provider_from_model_pickle_has_no_fork_asset() {
        assert_eq!(get_provider_from_model("big-pickle"), None);
        assert_eq!(get_provider_from_model("pickle-3"), None);
    }

    // ========== calculate_intensity edge case tests ==========

    #[test]
    fn test_calculate_intensity_zero_max_cost() {
        assert_eq!(calculate_intensity(50.0, 0.0), 0);
    }

    #[test]
    fn test_calculate_intensity_zero_cost() {
        assert_eq!(calculate_intensity(0.0, 100.0), 0);
    }

    #[test]
    fn test_calculate_intensity_both_zero() {
        assert_eq!(calculate_intensity(0.0, 0.0), 0);
    }

    #[test]
    fn test_calculate_intensity_equal_cost_max() {
        // ratio = 1.0, which is >= 0.75 → intensity 4
        assert_eq!(calculate_intensity(100.0, 100.0), 4);
    }

    #[test]
    fn test_calculate_intensity_exact_boundary_075() {
        assert_eq!(calculate_intensity(75.0, 100.0), 4);
    }

    #[test]
    fn test_calculate_intensity_exact_boundary_050() {
        assert_eq!(calculate_intensity(50.0, 100.0), 3);
    }

    #[test]
    fn test_calculate_intensity_exact_boundary_025() {
        assert_eq!(calculate_intensity(25.0, 100.0), 2);
    }

    #[test]
    fn test_calculate_intensity_just_below_025() {
        assert_eq!(calculate_intensity(24.99, 100.0), 1);
    }

    #[test]
    fn test_calculate_intensity_cost_exceeds_max() {
        // ratio > 1.0, still >= 0.75 → intensity 4
        assert_eq!(calculate_intensity(200.0, 100.0), 4);
    }

    #[test]
    fn test_calculate_intensity_tiny_fraction() {
        assert_eq!(calculate_intensity(0.001, 100.0), 1);
    }
}
