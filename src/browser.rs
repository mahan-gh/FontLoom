use headless_chrome::{Browser, LaunchOptions};

use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BROWSER_IDLE_TIME: Duration = Duration::from_secs(10);

use anyhow;

pub enum AppError {
    BrowserError(String),
    ProcessingError(String),
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        AppError::BrowserError(err.to_string())
    }
}

pub struct BrowserManager {
    browser: Arc<Mutex<Option<Browser>>>,
}

impl std::fmt::Debug for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::ProcessingError(msg) => write!(f, "Image Processing Error: {}", msg),
            AppError::BrowserError(msg) => write!(f, "Image Processing Error: {}", msg),
        }
    }
}

impl BrowserManager {
    pub fn new() -> Self {
        let browser_arc: Arc<Mutex<Option<Browser>>> = Arc::new(Mutex::new(None));

        Self {
            browser: browser_arc,
        }
    }

    pub fn get_or_create_browser(&self) -> Result<Browser, AppError> {
        let mut browser_lock = self.browser.lock().unwrap();

        if let Some(ref browser) = *browser_lock {
            if self.is_browser_connected(browser) {
                println!("Browser connected, returning existing instance");
                return Ok(browser.clone());
            } else {
                println!("Browser disconnected, creating new instance");
                *browser_lock = None;
            }
        }

        println!("Creating new browser instance");
        let new_browser = self.create_browser()?;
        *browser_lock = Some(new_browser.clone());

        Ok(new_browser)
    }

    /// Create a new browser instance with specified options
    fn create_browser(&self) -> Result<Browser, AppError> {
        let launch_options = LaunchOptions::default_builder()
            .headless(true)
            .idle_browser_timeout(BROWSER_IDLE_TIME)
            .sandbox(false)
            .args(vec![
                OsStr::new("--incognito"),
                OsStr::new("--hide-scrollbars"),
                OsStr::new("--disable-gpu"),
                OsStr::new("--no-first-run"),
                OsStr::new("--no-default-browser-check"),
            ])
            .build()
            .map_err(|e| {
                AppError::ProcessingError(format!("Failed to build launch options: {}", e))
            })?;

        let browser = Browser::new(launch_options)
            .map_err(|e| AppError::ProcessingError(format!("Failed to launch browser: {}", e)))?;

        eprintln!(
            "Created new browser instance with PID: {:?}",
            browser.get_process_id()
        );

        Ok(browser)
    }

    pub fn is_browser_connected(&self, browser: &Browser) -> bool {
        browser.get_version().is_ok()
    }

    pub fn terminate(&self) -> Result<(), AppError> {
        let mut browser_lock = self.browser.lock().unwrap();

        if let Some(browser) = browser_lock.take() {
            if let Some(pid) = browser.get_process_id() {
                println!("Terminating browser with PID: {}", pid);

                #[cfg(unix)]
                {
                    let _ = std::process::Command::new("kill")
                        .args(["-9", &pid.to_string()])
                        .output();
                }

                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/T", "/PID", &pid.to_string()])
                        .output();
                }
            }
            drop(browser);
        }

        Ok(())
    }

    /// Get current browser without creating new one
    pub fn get_browser(&self) -> Option<Browser> {
        let browser_lock = self.browser.lock().unwrap();
        browser_lock.as_ref().and_then(|b| {
            if self.is_browser_connected(b) {
                Some(b.clone())
            } else {
                None
            }
        })
    }

    /// Force close and recreate browser
    pub fn recreate_browser(&self) -> Result<Browser, AppError> {
        self.terminate()?;
        self.get_or_create_browser()
    }
}

impl Drop for BrowserManager {
    fn drop(&mut self) {
        println!("Dropping BrowserManager...");

        println!("Terminating browser process...");
        if let Err(e) = self.terminate() {
            println!("Error terminating browser on drop: {:?}", e);
        }
        println!("BrowserManager dropped.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::thread;

    /// Helper function to check if a process is running
    fn is_process_running(pid: u32) -> bool {
        #[cfg(unix)]
        {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        }

        #[cfg(windows)]
        {
            Command::new("tasklist")
                .args(["/FI", &format!("PID eq {}", pid), "/NH"])
                .output()
                .map(|output| {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    stdout.contains(&pid.to_string()) && !stdout.contains("INFO: No tasks")
                })
                .unwrap_or(false)
        }
    }

    /// Wait for process to terminate with timeout
    fn wait_for_process_termination(pid: u32, timeout_secs: u64) -> bool {
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        while start.elapsed() < timeout {
            if !is_process_running(pid) {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }

        false
    }

    /// Helper function to count Chrome processes
    fn count_chrome_processes() -> usize {
        #[cfg(unix)]
        {
            Command::new("pgrep")
                .args(["-f", "chrome|chromium"])
                .output()
                .map(|output| {
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .filter(|line| !line.is_empty())
                        .count()
                })
                .unwrap_or(0)
        }

        #[cfg(windows)]
        {
            Command::new("tasklist")
                .output()
                .map(|output| {
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .filter(|line| line.to_lowercase().contains("chrome"))
                        .count()
                })
                .unwrap_or(0)
        }
    }

    #[test]
    fn test_browser_lifecycle() -> Result<(), AppError> {
        let initial_chrome_count = count_chrome_processes();
        println!("Initial Chrome processes: {}", initial_chrome_count);

        let manager = BrowserManager::new();

        // First call creates browser
        let browser1 = manager.get_or_create_browser()?;
        let pid1 = browser1.get_process_id().expect("Should have PID");
        println!("Created browser with PID: {}", pid1);

        // Verify process is running
        assert!(
            is_process_running(pid1),
            "Browser process should be running"
        );

        // Second call returns same browser
        let browser2 = manager.get_or_create_browser()?;
        let pid2 = browser2.get_process_id();
        assert_eq!(Some(pid1), pid2, "Should reuse same browser instance");

        // Terminate and verify process is killed
        manager.terminate()?;
        println!("Terminated browser PID: {}", pid1);

        // Wait for process to terminate (Windows can take longer)
        assert!(
            wait_for_process_termination(pid1, 3),
            "Browser process {} should be terminated within 3 seconds",
            pid1
        );

        // Create new browser
        let browser3 = manager.get_or_create_browser()?;
        let pid3 = browser3.get_process_id().expect("Should have PID");
        println!("Created new browser with PID: {}", pid3);

        assert_ne!(pid1, pid3, "New browser should have different PID");
        assert!(
            is_process_running(pid3),
            "New browser process should be running"
        );

        // Clean up
        manager.terminate()?;
        assert!(
            wait_for_process_termination(pid3, 3),
            "Final browser process {} should be terminated",
            pid3
        );

        Ok(())
    }

    #[test]
    fn test_drop_terminates_browser() -> Result<(), AppError> {
        let initial_chrome_count = count_chrome_processes();
        println!("Initial Chrome processes: {}", initial_chrome_count);

        let pid = {
            let manager = BrowserManager::new();
            let browser = manager.get_or_create_browser()?;
            let pid = browser.get_process_id().expect("Should have PID");
            println!("Created browser with PID: {}", pid);

            assert!(is_process_running(pid), "Browser should be running");
            pid
            // manager goes out of scope here, Drop should be called
        };

        // Wait for process to terminate after Drop
        assert!(
            wait_for_process_termination(pid, 3),
            "Browser {} should be terminated after Drop",
            pid
        );

        Ok(())
    }

    #[test]
    fn test_multiple_recreate_no_leaks() -> Result<(), AppError> {
        let manager = BrowserManager::new();
        let mut pids = Vec::new();

        // Create and terminate multiple browsers
        for i in 0..3 {
            let browser = manager.get_or_create_browser()?;
            let pid = browser.get_process_id().expect("Should have PID");
            println!("Iteration {}: Created browser with PID: {}", i, pid);

            assert!(is_process_running(pid), "Browser should be running");
            pids.push(pid);

            manager.terminate()?;

            assert!(
                wait_for_process_termination(pid, 3),
                "Browser {} should be terminated in iteration {}",
                pid,
                i
            );
        }

        // Verify all PIDs are unique
        let unique_pids: std::collections::HashSet<_> = pids.iter().collect();
        assert_eq!(pids.len(), unique_pids.len(), "All PIDs should be unique");

        // Verify none of the old processes are still running
        for pid in pids {
            assert!(
                !is_process_running(pid),
                "Old browser PID {} should not be running",
                pid
            );
        }

        Ok(())
    }

    #[test]
    fn test_connection_check_after_manual_kill() -> Result<(), AppError> {
        let manager = BrowserManager::new();
        let browser = manager.get_or_create_browser()?;
        let pid = browser.get_process_id().expect("Should have PID");

        println!("Created browser with PID: {}", pid);
        assert!(
            manager.is_browser_connected(&browser),
            "Browser should be connected"
        );

        // Manually kill the process (simulating crash)
        #[cfg(unix)]
        {
            Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output()
                .expect("Failed to kill process");
        }

        #[cfg(windows)]
        {
            Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output()
                .expect("Failed to kill process");
        }

        // Wait for kill to complete
        assert!(
            wait_for_process_termination(pid, 3),
            "Process {} should be killed",
            pid
        );

        // Connection check should fail
        assert!(
            !manager.is_browser_connected(&browser),
            "Browser should be disconnected after manual kill"
        );

        // get_or_create should create new browser
        let new_browser = manager.get_or_create_browser()?;
        let new_pid = new_browser.get_process_id().expect("Should have PID");

        assert_ne!(pid, new_pid, "Should create new browser with different PID");
        assert!(is_process_running(new_pid), "New browser should be running");

        manager.terminate()?;
        assert!(
            wait_for_process_termination(new_pid, 3),
            "New browser {} should be terminated",
            new_pid
        );

        Ok(())
    }
}
