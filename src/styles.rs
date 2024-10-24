use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageOutputFormat, Pixel, Rgb};
use rand::seq::SliceRandom;
use rand::{thread_rng, Rng};
use tokio::fs as async_fs;
use tokio::fs::File as AsyncFile;
use tokio::io::AsyncReadExt;

use std::io::Cursor;
use std::path::{Path, PathBuf};

type Color = (u8, u8, u8);

fn random_color() -> Color {
    let mut rng = thread_rng();
    (rng.gen(), rng.gen(), rng.gen())
}

fn calc_mean_color(c1: &Color, c2: &Color) -> Color {
    (
        ((c1.0 as u16 + c2.0 as u16) / 2) as u8,
        ((c1.1 as u16 + c2.1 as u16) / 2) as u8,
        ((c1.2 as u16 + c2.2 as u16) / 2) as u8,
    )
}

fn ensure_contrast(c1: &Color, c2: &Color, threshold: &f64) -> bool {
    color_distance(c1, c2) > *threshold
}

fn color_distance(c1: &Color, c2: &Color) -> f64 {
    let (r1, g1, b1) = c1;
    let (r2, g2, b2) = c2;
    ((*r1 as f64 - *r2 as f64).powi(2)
        + (*g1 as f64 - *g2 as f64).powi(2)
        + (*b1 as f64 - *b2 as f64).powi(2))
    .sqrt()
}

fn relative_luminance(rgb: &Color) -> f64 {
    let (r, g, b) = rgb;
    let channel_luminance = |c: f64| {
        let c = c / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel_luminance(*r as f64)
        + 0.7152 * channel_luminance(*g as f64)
        + 0.0722 * channel_luminance(*b as f64)
}

fn contrast_ratio(rgb1: &Color, rgb2: &Color) -> f64 {
    let lum1 = relative_luminance(rgb1);
    let lum2 = relative_luminance(rgb2);
    let (lighter, darker) = if lum1 > lum2 {
        (lum1, lum2)
    } else {
        (lum2, lum1)
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn ensure_wcag_contrast(bg_color: &Color, text_color: &Color, ratio: &f64) -> bool {
    contrast_ratio(bg_color, text_color) >= *ratio
}

fn calc_mean_image(buffer: &[u8]) -> Result<Color, String> {
    let img =
        image::load_from_memory(buffer).map_err(|e| (format!("Failed to load image: {}", e)))?;
    let (r_sum, g_sum, b_sum, pixel_count) = img.pixels().fold(
        (0u64, 0u64, 0u64, 0u64),
        |(r, g, b, count), (_, _, pixel)| {
            let rgb = pixel.to_rgb();
            (
                r + rgb[0] as u64,
                g + rgb[1] as u64,
                b + rgb[2] as u64,
                count + 1,
            )
        },
    );
    Ok((
        (r_sum / pixel_count) as u8,
        (g_sum / pixel_count) as u8,
        (b_sum / pixel_count) as u8,
    ))
}

fn generate_noise_image() -> Result<String, String> {
    let width = thread_rng().gen_range(100..=1000);
    let height = thread_rng().gen_range(100..=1000);
    let noise_level = thread_rng().gen_range(0.1..=0.9);

    let img = ImageBuffer::from_fn(width, height, |_, _| {
        let noise = || (thread_rng().gen::<f32>() * 255.0 * noise_level) as u8;
        Rgb([noise(), noise(), noise()])
    });

    let mut buffer = Cursor::new(Vec::new());
    img.write_to(&mut buffer, ImageOutputFormat::Png)
        .map_err(|e| format!("Failed to write image: {}", e))?;

    Ok(format!(
        "data:image/png;base64,{}",
        STANDARD.encode(buffer.get_ref())
    ))
}

async fn select_image(images: &Vec<PathBuf>) -> Result<(image::DynamicImage, u32, u32), String> {
    let img_path = images.choose(&mut thread_rng()).unwrap();
    let buffer = async_fs::read(img_path)
        .await
        .map_err(|_| "Error reading image file".to_string())?;

    let img: image::DynamicImage =
        image::load_from_memory(&buffer).map_err(|e| format!("Failed to load image: {}", e))?;
    let (width, height) = img.dimensions();

    Ok((img, width, height))
}

async fn generate_background_style(images: &Vec<PathBuf>) -> Result<(String, String), String> {
    let use_image_bg = thread_rng().gen_bool(0.5);
    let use_overlay = thread_rng().gen_bool(0.3); // Add probability for an overlay

    if use_image_bg {
        let mut img: DynamicImage;
        let mut width: u32;
        let mut height: u32;

        let mut attempts = 0;
        let max_attempts = 10;

        (img, width, height) = select_image(images).await?;

        while (width <= 350 || height <= 350) && attempts < max_attempts {
            (img, width, height) = select_image(&images).await?;
            attempts += 1;
        }

        let crop_width = thread_rng().gen_range(380..=width.min(1500));
        let crop_height = thread_rng().gen_range(380..=height.min(1500));

        let left = thread_rng().gen_range(0..(width - crop_width + 1));
        let top = thread_rng().gen_range(0..(height - crop_height + 1));

        let cropped_image = img.crop(left, top, crop_width, crop_height);
        let mut buffer = Cursor::new(Vec::new());
        cropped_image
            .write_to(&mut buffer, ImageOutputFormat::Png)
            .map_err(|e| format!("Failed to write image: {}", e))?;

        let base64_cropped = STANDARD.encode(&buffer.get_ref()[..]);
        let mut bg_style = format!(
            "background-image: url(data:image/png;base64,{}); background-size: cover; background-position: center;",
            base64_cropped
        );

        // Add overlay pattern on top of the image
        if use_overlay {
            let overlay_color = random_color();
            let opacity = thread_rng().gen_range(0.05..0.35);
            bg_style = format!(
                "{} background: linear-gradient(rgba({},{},{},{}), rgba({},{},{},{})), {}",
                bg_style,
                overlay_color.0,
                overlay_color.1,
                overlay_color.2,
                opacity,
                overlay_color.0,
                overlay_color.1,
                overlay_color.2,
                opacity,
                bg_style
            );
        }

        let mut text_color = random_color();
        let bg_color = calc_mean_image(buffer.get_ref()).map_err(|e| format!("Error: {}", e))?;
        while !ensure_wcag_contrast(&bg_color, &text_color, &3.0) {
            text_color = random_color();
        }

        Ok((
            bg_style,
            format!(
                "#{:02x}{:02x}{:02x}",
                text_color.0, text_color.1, text_color.2
            ),
        ))
    } else {
        let use_gradient = thread_rng().gen_bool(0.3); // 30% chance to use gradient

        if use_gradient {
            let color1 = random_color();
            let color2 = random_color();

            let mean_color = calc_mean_color(&color1, &color1);
            let mut text_color = random_color();
            // while !ensure_wcag_contrast(color1, text_color, 3.0)
            //     || !ensure_wcag_contrast(color2, text_color, 3.0)
            // {
            while !ensure_wcag_contrast(&mean_color, &text_color, &3.0) {
                text_color = random_color();
            }
            Ok((
                format!(
                    "background: linear-gradient(45deg, #{:02x}{:02x}{:02x}, #{:02x}{:02x}{:02x});",
                    color1.0, color1.1, color1.2, color2.0, color2.1, color2.2
                ),
                format!(
                    "#{:02x}{:02x}{:02x}",
                    text_color.0, text_color.1, text_color.2
                ),
            ))
        } else {
            let bg_color = random_color();

            let mut text_color = random_color();
            while !ensure_wcag_contrast(&bg_color, &text_color, &3.0) {
                text_color = random_color();
            }
            Ok((
                format!(
                    "background-color: #{:02x}{:02x}{:02x};",
                    bg_color.0, bg_color.1, bg_color.2
                ),
                format!(
                    "#{:02x}{:02x}{:02x}",
                    text_color.0, text_color.1, text_color.2
                ),
            ))
        }
    }
}

fn generate_style_properties() -> String {
    let random_prop = |prob: f64, range: (f64, f64), decimals: usize| -> f64 {
        if thread_rng().gen::<f64>() < prob {
            let value = thread_rng().gen_range(range.0..=range.1);
            (value * 10f64.powi(decimals as i32)).round() / 10f64.powi(decimals as i32)
        } else {
            0.0
        }
    };

    let props = [
        ("skew", 0.5, (-7.0, 7.0), 2),
        ("rotate", 0.5, (-7.0, 7.0), 2),
        ("translate", 0.4, (-4.0, 4.0), 2),
        ("blur", 0.35, (0.0, 0.4), 2),
        ("brightness", 0.4, (0.8, 1.2), 1),
        ("contrast", 0.4, (0.8, 1.2), 1),
    ];

    let transform = props
        .iter()
        .take(3)
        .map(|(name, prob, range, decimals)| {
            let x = random_prop(*prob, *range, *decimals);
            let y = if *name == "rotate" {
                0.0
            } else {
                random_prop(*prob, *range, *decimals)
            };
            if *name == "translate" {
                format!("{}({}px, {}px)", name, x, y)
            } else if *name == "rotate" {
                format!("{}({}deg)", name, x)
            } else {
                format!("{}({}deg, {}deg)", name, x, y)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let filter = props
        .iter()
        .skip(3)
        .map(|(name, prob, range, decimals)| {
            let value = random_prop(*prob, *range, *decimals).max(1.0);
            if *name == "blur" {
                format!("{}({}px)", name, value)
            } else {
                format!("{}({})", name, value)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let width = thread_rng().gen_range(250..=600);
    let height = thread_rng().gen_range(200..=450);
    let font_size = thread_rng().gen_range(32..=100);
    let text_align = ["center", "left", "right"]
        .choose(&mut thread_rng())
        .unwrap();

    let padding = thread_rng().gen_range(10..=40);
    let margin = thread_rng().gen_range(20..=60);

    format!(
        "width: {}px; height: {}px; font-size: {}px; text-align: {}; transform: {}; filter: {}; padding: {}px; margin: {}px;",
        width, height, font_size, text_align, transform, filter, padding, margin
    )
}
fn generate_shadow_style(bg_style: &str, text_color: &str) -> String {
    if thread_rng().gen_bool(0.4) {
        let bg_color = parse_color(bg_style);
        let text_color = parse_color(text_color);
        let mut shadow_color = random_color();
        let mean_color = calc_mean_color(&bg_color, &text_color);
        while !ensure_contrast(&mean_color, &shadow_color, &3.0) {
            // while !ensure_contrast(bg_color, shadow_color, 3.0)
            //     || !ensure_contrast(text_color, shadow_color, 3.0)
            // {
            shadow_color = random_color();
        }

        let shadow_x = thread_rng().gen_range(-5.0..=6.0);
        let shadow_y = thread_rng().gen_range(-5.0..=6.0);
        let shadow_blur = thread_rng().gen_range(1.0..=8.0);
        format!(
            "text-shadow: {:.2}px {:.2}px {:.2}px #{:02x}{:02x}{:02x};",
            shadow_x, shadow_y, shadow_blur, shadow_color.0, shadow_color.1, shadow_color.2
        )
    } else {
        String::new()
    }
}

fn generate_outline_style(bg_style: &str, text_color: &str) -> String {
    if thread_rng().gen_bool(0.2) {
        let bg_color = parse_color(bg_style);
        let text_color = parse_color(text_color);
        let mut outline_color = random_color();
        let mean_color = calc_mean_color(&bg_color, &text_color);
        while !ensure_contrast(&mean_color, &outline_color, &3.0) {
            // while !ensure_contrast(bg_color, outline_color, 3.0)
            //     || !ensure_contrast(text_color, outline_color, 3.0)
            // {
            outline_color = random_color();
        }

        let outline_width = thread_rng().gen_range(1.0..=3.0);
        format!(
            "-webkit-text-stroke: {:.2}px #{:02x}{:02x}{:02x};",
            outline_width, outline_color.0, outline_color.1, outline_color.2
        )
    } else {
        String::new()
    }
}

fn generate_noise_style() -> String {
    if thread_rng().gen_bool(0.4) {
        let noise_image = generate_noise_image().unwrap_or_default();
        let noise_intensity = thread_rng().gen_range(0.1..=0.3);
        format!(
            "body::after {{ content: ''; position: absolute; top: 0; left: 0; width: 100%; height: 100%; background-image: url({}); opacity: {:.2}; pointer-events: none; z-index: -1; }}",
            noise_image, noise_intensity
        )
    } else {
        String::new()
    }
}

fn parse_color(color_str: &str) -> Color {
    let color_str = color_str.trim_start_matches('#');
    if color_str.len() == 6 {
        (
            u8::from_str_radix(&color_str[0..2], 16).unwrap_or(0),
            u8::from_str_radix(&color_str[2..4], 16).unwrap_or(0),
            u8::from_str_radix(&color_str[4..6], 16).unwrap_or(0),
        )
    } else {
        (0, 0, 0)
    }
}

async fn generate_random_styles(images: &Vec<PathBuf>) -> Result<String, String> {
    // Step 1: Generate background style and text color
    let (bg_style, text_color_hex) = generate_background_style(&images).await?;

    // Step 2: Generate other style properties
    let style_properties = generate_style_properties();

    // Step 3: Generate shadow style
    let shadow_style = generate_shadow_style(&bg_style, &text_color_hex);

    // Step 4: Generate outline style
    let outline_style = generate_outline_style(&bg_style, &text_color_hex);

    // Step 5: Generate noise style
    let noise_style = generate_noise_style();

    // Combine all styles into one CSS string
    let styles = format!(
        "
        {}
        color: {};
        position: relative;
        z-index: 0;
        {}
        {}
        {}
        ",
        bg_style, text_color_hex, style_properties, shadow_style, outline_style
    );

    Ok(styles + &noise_style)
}

async fn convert_font_to_base64(font_path: &str) -> Result<String, std::io::Error> {
    let mut file = AsyncFile::open(font_path).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;
    let encoded = STANDARD.encode(&buffer);

    Ok(encoded)
}

pub async fn create_html_content(
    template: &str,
    phrase: &str,
    font_file: &str,
    images: &Vec<PathBuf>,
    method: Option<&str>,
) -> Result<String, std::io::Error> {
    // Use &Path instead of allocating a new PathBuf object
    let font_path = Path::new(font_file);

    let font_name = font_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default_font"); // No need to convert to String here

    // Directly convert the font to base64 without unnecessary allocation
    let base64_font = convert_font_to_base64(font_file).await?;

    // Generate styles based on the method option
    let styles = match method {
        Some("simple") => {
            "background-color: white; color: black; text-align: center; font-size: 50px;"
        }
        _ => &match generate_random_styles(&images).await {
            Ok(style_string) => style_string, // Return the owned String directly
            Err(_) => "default-style".to_string(), // Fallback to a default owned String
        },
    };

    // Generate a random value to choose between two HTML content versions
    let random_value = thread_rng().gen_range(0..2);

    // Build the HTML content with less string replacement operations
    let html_content = if random_value == 0 {
        template
            .replace("{phrase}", phrase)
            .replace("{base64_font}", &base64_font)
            .replace("{font_name}", font_name)
            .replace("{text_styles}", styles)
            .replace("{body_styles}", "")
    } else {
        template
            .replace("{phrase}", phrase)
            .replace("{base64_font}", &base64_font)
            .replace("{font_name}", font_name)
            .replace("{text_styles}", "")
            .replace("{body_styles}", styles)
    };

    Ok(html_content)
}
