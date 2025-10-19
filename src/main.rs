mod browser;
mod styles;
use crate::browser::BrowserManager;
use crate::styles::create_html_content;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use colored::*;
use futures::future::join_all;
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::types::Bounds;
use headless_chrome::Tab;
use rand::seq::SliceRandom;
use rand::{thread_rng, Rng};
use serde_json::Value;
use tokio::fs as async_fs;
use tokio::fs::File as AsyncFile;
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, Semaphore};

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

const IMAGES_PER_FONT: usize = 5;
const OUTPUT_DIR: &str = "./data";
const FONTS_DIR: &str = "./fonts";
const TEMPLATE_PATH: &str = "./index.html";
const PHRASES_PATH: &str = "../dataGenerator/texts/phrases.json";
const IMAGE_FOLDER: &str = "../dataGenerator/background";

static COUNTER: AtomicU64 = AtomicU64::new(0);

async fn convert_font_to_base64(font_path: &str) -> Result<String, std::io::Error> {
    let mut file = AsyncFile::open(font_path).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;
    let encoded = STANDARD.encode(&buffer);

    Ok(encoded)
}

async fn get_font_vector(font_dir: &str) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
    let mut font_data = Vec::new();
    let mut font_files = async_fs::read_dir(font_dir).await?;

    while let Some(entry) = font_files.next_entry().await? {
        let base64_font = convert_font_to_base64(entry.path().to_str().unwrap()).await?;
        font_data.push(base64_font);
    }

    Ok(font_data)
}

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

async fn get_image_buffers() -> Result<Vec<Arc<Vec<u8>>>, String> {
    let mut entries = async_fs::read_dir(IMAGE_FOLDER)
        .await
        .map_err(|_| format!("Error reading folder '{}'", IMAGE_FOLDER))?;

    let mut image_buffers = Vec::new();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| "Error reading directory entries")?
    {
        let path = entry.path();
        if path.is_file() {
            // Read the entire file into a buffer
            let mut file = AsyncFile::open(&path)
                .await
                .map_err(|e| format!("Error opening file {:?}: {}", path, e))?;

            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .await
                .map_err(|e| format!("Error reading file {:?}: {}", path, e))?;

            image_buffers.push(Arc::from(buffer));
        }
    }

    if image_buffers.is_empty() {
        return Err("No images found in folder.".to_string());
    }

    Ok(image_buffers)
}

struct TabPool {
    // We protect the vector with a mutex because taking/returning a tab is fast
    // and does not require async operations.
    tabs: Arc<Mutex<Vec<Arc<Tab>>>>,
    // Semaphore limits concurrent leases to the pool capacity.
    semaphore: Arc<Semaphore>,
    browser_manager: Arc<Mutex<BrowserManager>>,
    capacity: usize,
    recreating: Arc<Mutex<bool>>,
}

impl TabPool {
    async fn new(
        // browser: Arc<Mutex<Browser>>,
        capacity: usize,
    ) -> Result<Arc<Self>, Box<dyn Error + Send + Sync>> {
        let browser_manager = Arc::new(Mutex::new(BrowserManager::new()));
        let mut created_tabs: Vec<Arc<Tab>> = Vec::with_capacity(capacity);

        {
            let manager = browser_manager.lock().await;
            let browser = manager.get_or_create_browser().unwrap();
            for _ in 0..capacity {
                let tab = browser.new_tab()?;
                created_tabs.push(tab);
            }
        }

        Ok(Arc::new(Self {
            tabs: Arc::from(Mutex::new(created_tabs)),
            semaphore: Arc::new(Semaphore::new(capacity)),
            browser_manager,
            capacity,
            recreating: Arc::new(Mutex::new(false)),
        }))
    }

    /// Check if a tab is still valid (browser not terminated)
    async fn is_tab_valid(&self, tab: &Tab) -> bool {
        // Try a simple operation to check if the tab is still alive
        // This depends on your Tab API - adjust accordingly
        tab.bring_to_front().is_ok()
    }

    /// Recreate all tabs when browser is terminated
    async fn recreate_tabs(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        // Check if already recreating to prevent concurrent recreation
        {
            let mut recreating = self.recreating.lock().await;
            if *recreating {
                // Another task is already recreating, wait for it
                return Ok(());
            }
            *recreating = true;
        }

        // Ensure recreation flag is reset even if we error
        let _guard = RecreationGuard {
            recreating: self.recreating.clone(),
        };

        let manager = self.browser_manager.lock().await;
        let browser = manager.get_or_create_browser().unwrap();

        let mut new_tabs = Vec::with_capacity(self.capacity);
        for _ in 0..self.capacity {
            let tab = browser.new_tab()?;
            new_tabs.push(tab);
        }

        // Replace old tabs with new ones
        let mut tabs_guard = self.tabs.lock().await;
        *tabs_guard = new_tabs;

        Ok(())
    }

    async fn acquire(self: &Arc<Self>) -> Result<TabLease, Box<dyn Error + Send + Sync>> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| format!("Failed to acquire tab permit: {}", e))?;

        {
            let manager = self.browser_manager.lock().await;
            if manager.get_browser().is_none() {
                drop(manager); // Release lock before recreating
                self.recreate_tabs().await?;
            }
        }

        let tab = {
            let mut tabs_guard = self.tabs.lock().await;

            if tabs_guard.len() == 0 {
                drop(tabs_guard); // Release lock before recreating
                self.recreate_tabs().await?;

                let mut tabs_guard = self.tabs.lock().await;
                tabs_guard.pop()
            } else {
                tabs_guard.pop()
            }
        };

        let mut tab = match tab {
            Some(t) => t,
            None => return Err("Tab pool underflow: no tab available despite permit".into()),
        };

        if !self.is_tab_valid(&tab).await {
            self.recreate_tabs().await?;

            let mut tabs_guard = self.tabs.lock().await;
            tab = tabs_guard
                .pop()
                .ok_or("Failed to acquire tab after recreation")?;
        }

        Ok(TabLease {
            pool: self.clone(),
            tab: Some(tab),
            _permit: permit,
        })
    }
}

struct TabLease {
    pool: Arc<TabPool>,
    tab: Option<Arc<Tab>>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl TabLease {
    fn tab(&self) -> &Tab {
        self.tab.as_ref().expect("tab lease without tab").as_ref()
    }

    /// Execute an operation with automatic retry on browser termination
    async fn execute_with_retry<F, R>(
        &mut self,
        mut operation: F,
    ) -> Result<R, Box<dyn Error + Send + Sync>>
    where
        F: FnMut(&Tab) -> Result<R, Box<dyn Error + Send + Sync>>,
    {
        const MAX_RETRIES: usize = 3;

        for attempt in 0..MAX_RETRIES {
            match operation(self.tab()) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Check if error is due to browser termination
                    if attempt < MAX_RETRIES - 1 && self.is_browser_error(&e) {
                        // Recreate tabs and get a new one
                        self.pool.recreate_tabs().await?;

                        // Get a fresh tab
                        let mut tabs_guard = self.pool.tabs.lock().await;
                        if let Some(new_tab) = tabs_guard.pop() {
                            self.tab = Some(new_tab);
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }

        Err("Max retries exceeded".into())
    }

    fn is_browser_error(&self, error: &Box<dyn Error + Send + Sync>) -> bool {
        // Customize this based on your error types
        let error_msg = error.to_string().to_lowercase();
        error_msg.contains("browser")
            || error_msg.contains("connection")
            || error_msg.contains("closed")
            || error_msg.contains("terminated")
    }
}

impl Drop for TabLease {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let tab = self.tab.take();

        tokio::spawn(async move {
            if let Some(tab) = tab {
                // Only return valid tabs to the pool
                let is_valid = pool.is_tab_valid(&tab).await;
                if is_valid {
                    let mut tabs_guard = pool.tabs.lock().await;
                    tabs_guard.push(tab);
                }
            }
        });
    }
}
/// Guard to ensure recreation flag is reset
struct RecreationGuard {
    recreating: Arc<Mutex<bool>>,
}

impl Drop for RecreationGuard {
    fn drop(&mut self) {
        let recreating = self.recreating.clone();
        tokio::spawn(async move {
            let mut flag = recreating.lock().await;
            *flag = false;
        });
    }
}

async fn process_font(
    font: &str,
    phrase_assignments: &Vec<String>,
    html_template: &String,
    images: &Vec<Arc<Vec<u8>>>,
    tab_pool: Arc<TabPool>,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let font_dir = format!("{}/{}", FONTS_DIR, font);
    let base64_fonts = get_font_vector(&font_dir).await?;

    let lease = tab_pool.acquire().await?;
    let tab = lease.tab();

    for phrase in phrase_assignments {
        let base64_font = base64_fonts.choose(&mut thread_rng()).unwrap();

        let html_content =
            create_html_content(&font, &html_template, &phrase, &base64_font, &images, None)
                .await
                .expect("failed to generate html content");

        if let Err(e) = create_image(&tab, &html_content, &font).await {
            eprintln!("Error creating image for font {}: {}", font, e);
            continue;
        }
    }

    Ok(format!(
        "{} {}!",
        "Created the data for".green(),
        font.red()
    ))
}

async fn create_image(tab: &Tab, html_content: &str, font: &str) -> Result<(), Box<dyn Error>> {
    let width = thread_rng().gen_range(400..1000) as f64;
    let height = thread_rng().gen_range(400..1000) as f64;
    let quality = thread_rng().gen_range(75..100);

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

    let screenshot = tab
        .capture_screenshot(
            CaptureScreenshotFormatOption::Jpeg,
            Some(quality),
            None,
            true,
        )
        .map_err(|e| format!("Failed to capture screenshot: {}", e))?;

    async_fs::write(&output_image, &screenshot)
        .await
        .map_err(|e| format!("Failed to write image file {}: {}", output_image, e))?;

    Ok(())
}

async fn async_main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let start = Instant::now();

    let (fonts_result, template_result, phrases_result, images_result) = tokio::join!(
        get_available_fonts(FONTS_DIR),
        async_fs::read_to_string(TEMPLATE_PATH),
        load_phrases(PHRASES_PATH),
        get_image_buffers()
    );

    let available_fonts = fonts_result?;
    let html_template = template_result?;
    let phrase_list = phrases_result?;
    let image_buffers = images_result?;

    recreate_output_dir(OUTPUT_DIR, &available_fonts).await?;
    let phrase_assignments: HashMap<String, Vec<String>> =
        assign_phrases_to_fonts(&available_fonts, &phrase_list, IMAGES_PER_FONT);

    let image_buffers = Arc::new(image_buffers);
    let html_template = Arc::new(html_template);
    let available_fonts = Arc::new(available_fonts);
    let phrase_assignments = Arc::new(phrase_assignments);
    // let browser = Arc::from(Mutex::from(create_browser()));
    let tab_pool = TabPool::new(20).await?;

    let total_tasks = available_fonts.len();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<(usize, (bool, String))>(total_tasks);
    let mut handles = Vec::new();

    for (index, font) in available_fonts.iter().enumerate() {
        let html_template = Arc::clone(&html_template);
        let phrase_assignments = Arc::clone(&phrase_assignments);
        let image_buffers = Arc::clone(&image_buffers);
        let font = font.clone();
        let tx = tx.clone();
        // let browser = Arc::clone(&browser);
        let tab_pool = tab_pool.clone();

        let handle = tokio::spawn(async move {
            let result = if let Some(phrases) = phrase_assignments.get(&font) {
                match process_font(&font, &phrases, &html_template, &image_buffers, tab_pool).await
                {
                    Ok(msg) => (true, format!("result: {}", msg)),
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

            let progress = (completed as f32 / total_tasks as f32 * 100.0) as u32;

            println!("({}%) Task {} completed. {}", progress, index + 1, result);
        }

        println!("\nSummary:");
        println!("Total tasks completed: {}", completed);
        println!("Successful: {}", successful);
        println!("Failed: {}", failed);

        (completed, successful, failed)
    });

    let join_results = join_all(handles).await;
    drop(tx);
    let _ = printer_handle.await?;

    // Check for panics
    let panic_count = join_results.iter().filter(|res| res.is_err()).count();
    if panic_count > 0 {
        eprintln!("\nWarning: {} tasks panicked!", panic_count);
    }

    let elapsed = start.elapsed();
    let minutes = elapsed.as_secs() / 60;
    let seconds = elapsed.as_secs() % 60;
    println!(
        "Total time elapsed: {} minutes and {} seconds.",
        minutes, seconds
    );

    println!("{}", "All tasks completed!"); // .cyan()

    Ok(())
}

use tokio::runtime::Builder;
fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let runtime = Builder::new_multi_thread()
        .worker_threads(12)
        .thread_name("my-async-worker")
        .enable_all() // Enable all runtime features (I/O, time, etc.)
        .build()?;

    runtime.block_on(async_main())
}
