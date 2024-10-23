mod styles;
use crate::styles::create_html_content;

use colored::*;
use futures::future::join_all;
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::types::Bounds;
use headless_chrome::{Browser, LaunchOptions, Tab};
use rand::{thread_rng, Rng};
use serde_json::Value;
use tokio::fs as async_fs;
use tokio::sync::Semaphore;
// use tokio::task;
// use tokio::task::JoinHandle;

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
// use std::ffi::OsStr;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

const SEMAPHORES: usize = 12;
const CLASSES_LENGTH: usize = 100;
const OUTPUT_DIR: &str = "./data";
const FONTS_DIR: &str = "../dataGenerator/fonts_test";
const TEMPLATE_PATH: &str = "../dataGenerator/index_rs.html";
const PHRASES_PATH: &str = "../dataGenerator/texts/phrases.json";
const IMAGE_FOLDER: &str = "../dataGenerator/background";

static COUNTER: AtomicU64 = AtomicU64::new(0);

async fn recreate_output_dir(
    dir: &str,
    subfolders: &Vec<String>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir)?;

    for subfolder in subfolders {
        let subfolder_path = format!("{}/{}", dir, subfolder);
        fs::create_dir_all(&subfolder_path)?;
    }
    Ok(())
}

async fn get_available_fonts(fonts_dir: &str) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
    let paths = fs::read_dir(fonts_dir)?;
    let mut fonts = Vec::new();
    for path in paths {
        if let Ok(entry) = path {
            fonts.push(entry.file_name().into_string().unwrap());
        }
    }
    Ok(fonts)
}

async fn load_phrases(file_path: &str) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
    let file = fs::read_to_string(file_path)?;
    let phrases: Value = serde_json::from_str(&file)?;
    Ok(phrases
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect())
}

fn assign_phrases_to_fonts(
    fonts: &[String],
    phrases: &[String],
    limit: usize,
) -> HashMap<String, Vec<String>> {
    let mut assignments: HashMap<String, Vec<String>> = HashMap::new();
    let mut font_cycle = fonts.iter().cycle();

    for phrase in phrases {
        if let Some(font) = font_cycle.next() {
            let font_entry = assignments.entry(font.clone()).or_insert(Vec::new());
            if font_entry.len() < limit {
                font_entry.push(phrase.clone());
            }
        }
    }

    assignments
}

async fn read_font_files(font_dir: &str) -> Result<Vec<PathBuf>, Box<dyn Error + Send + Sync>> {
    let mut font_files = Vec::new();
    let mut dir_entries = async_fs::read_dir(font_dir).await?;

    while let Some(entry) = dir_entries.next_entry().await? {
        font_files.push(entry.path());
    }

    if font_files.is_empty() {
        return Err(format!("No font files found in directory: {}", font_dir).into());
    }

    Ok(font_files)
}

async fn process_font(
    font: &str,
    phrase_assignments: &Vec<String>,
    html_template: &str,
    images: &Vec<PathBuf>,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let font_dir = format!("{}/{}", FONTS_DIR, font);
    let font_files = read_font_files(&font_dir).await?;

    let browser = Browser::new(LaunchOptions {
        headless: true,
        enable_gpu: true,
        ..Default::default()
    })?;

    let tab = browser.new_tab()?;

    // Loop through each phrase for the current font
    for phrase in phrase_assignments {
        let font_file = font_files[thread_rng().gen_range(0..font_files.len())]
            .to_str()
            .ok_or("Failed to convert font file path to string")?;

        let html_content = create_html_content(&html_template, phrase, font_file, images, None)
            .await
            .expect("failed to generate html content");

        if let Err(e) = create_image(&tab, &html_content, font).await {
            eprintln!("Error creating image for font {}: {}", font, e);
            continue;
        }
    }

    tab.close(true)?;

    Ok(format!(
        "{} {}!",
        "Successfully created data for".green(),
        font.red()
    ))
}

async fn create_image(tab: &Tab, html_content: &str, font: &str) -> Result<(), Box<dyn Error>> {
    let width = thread_rng().gen_range(400..1000) as f64;
    let height = thread_rng().gen_range(350..900) as f64;
    let quality = thread_rng().gen_range(65..100);

    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    let output_image = format!("{}/{}/{}.jpg", OUTPUT_DIR, font, counter);

    tab.set_bounds(Bounds::Normal {
        left: None,
        top: None,
        width: Some(width),
        height: Some(height),
    })
    .map_err(|e| format!("Failed to set viewport bounds: {}", e))?;

    // let js = format!("document.write(`{}`);", html_content);
    let js = format!("document.body.innerHTML = `{}`;", html_content);
    tab.evaluate(js.as_str(), true)
        .map_err(|e| format!("Failed to inject HTML: {}", e))?;

    // Capture screenshot with error handling
    let screenshot = tab
        .capture_screenshot(
            CaptureScreenshotFormatOption::Jpeg,
            Some(quality),
            None,
            true,
        )
        .map_err(|e| format!("Failed to capture screenshot: {}", e))?;

    // Write file with error handling
    async_fs::write(&output_image, &screenshot)
        .await
        .map_err(|e| format!("Failed to write image file {}: {}", output_image, e))?;

    Ok(())
}

async fn get_image_paths() -> Result<Vec<PathBuf>, String> {
    let mut entries = async_fs::read_dir(IMAGE_FOLDER)
        .await
        .map_err(|_| format!("Error reading folder '{}'", IMAGE_FOLDER))?;

    let mut images = Vec::new();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| "Error reading directory entries")?
    {
        let path = entry.path();
        if path.is_file() {
            images.push(path);
        }
    }

    if images.is_empty() {
        return Err("No images found in folder.".to_string());
    }

    Ok(images)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let start = Instant::now();
    let available_fonts = get_available_fonts(FONTS_DIR).await?;
    let html_template = async_fs::read_to_string(TEMPLATE_PATH).await?;
    recreate_output_dir(OUTPUT_DIR, &available_fonts).await?;
    let phrase_list = load_phrases(PHRASES_PATH).await?;
    let phrase_assignments =
        assign_phrases_to_fonts(&available_fonts, &phrase_list, CLASSES_LENGTH);
    let images = get_image_paths().await?;

    // Share the font and phrase data across threads safely with Arc
    let images = Arc::new(images);
    let html_template = Arc::new(html_template);
    let available_fonts = Arc::new(available_fonts);
    let phrase_assignments = Arc::new(phrase_assignments);
    let semaphore = Arc::new(Semaphore::new(SEMAPHORES)); // Set max concurrent tasks to 10 (adjust based on your CPU cores)

    let total_tasks = available_fonts.len();
    let (tx, mut rx) = tokio::sync::mpsc::channel(total_tasks);

    let mut handles = Vec::new();
    let mut task_starts = HashMap::new();

    for (index, font) in available_fonts.iter().enumerate() {
        let html_template = Arc::clone(&html_template);
        let phrase_assignments = Arc::clone(&phrase_assignments);
        let semaphore = Arc::clone(&semaphore);
        let images = Arc::clone(&images);
        let font = font.clone();
        let tx = tx.clone();

        task_starts.insert(index, Instant::now());
        let handle = tokio::spawn(async move {
            let _permit = semaphore.acquire().await.unwrap();

            let result = if let Some(phrases) = phrase_assignments.get(&font) {
                match process_font(&font, &phrases, &html_template, &images).await {
                    Ok(msg) => (true, format!("Success: {}", msg)),
                    Err(e) => (false, format!("Error: {}", e)),
                }
            } else {
                (false, format!("No phrases assigned to font {}", font))
            };

            let _ = tx.send((index, result)).await;
        });

        handles.push(handle);
    }

    let mut completed = 0;
    let total_tasks = available_fonts.len();
    let mut successful = 0;
    let mut failed = 0;

    let printer_handle = tokio::spawn(async move {
        while let Some((index, (success, result))) = rx.recv().await {
            completed += 1;
            if success {
                successful += 1;
            } else {
                failed += 1;
            }

            let task_duration = task_starts
                .get(&index)
                .map(|start| start.elapsed())
                .unwrap_or_default();

            let progress = (completed as f32 / total_tasks as f32 * 100.0) as u32;

            println!(
                "({}%) Task {} completed in {:?}: {}",
                progress,
                index + 1,
                task_duration,
                result
            );
        }

        println!("\nSummary:");
        println!("Total tasks completed: {}", completed);
        println!("Successful: {}", successful);
        println!("Failed: {}", failed);
    });

    join_all(handles).await;
    drop(tx);
    printer_handle.await?;

    println!("Total time elapsed: {:?}", start.elapsed());
    println!("{}", "All tasks completed!".cyan());

    Ok(())
}