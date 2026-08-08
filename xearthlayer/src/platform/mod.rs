//! Platform-specific utilities for Linux and Windows

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "windows")]
pub mod windows;

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "windows")]
pub use windows::*;

/// Get the configuration directory for xearthlayer
pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("xearthlayer")
    }

    #[cfg(target_os = "windows")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("C:\\Users\\AppData\\Local"))
            .join("xearthlayer")
    }
}

/// Get the cache directory
pub fn cache_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("xearthlayer")
    }

    #[cfg(target_os = "windows")]
    {
        dirs::cache_dir()
            .unwrap_or_else(|| {
                dirs::config_dir()
                    .unwrap_or_else(|| PathBuf::from("C:\\Users\\AppData\\Local"))
            })
            .join("xearthlayer\\cache")
    }
}

/// Get the application data directory
pub fn app_data_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from(".local/share"))
            .join("xearthlayer")
    }

    #[cfg(target_os = "windows")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| {
                dirs::config_dir()
                    .unwrap_or_else(|| PathBuf::from("C:\\Users\\AppData\\Local"))
            })
            .join("xearthlayer")
    }
}

/// Create all required directories
pub fn ensure_directories() -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::create_dir_all(cache_dir())?;
    std::fs::create_dir_all(app_data_dir())?;
    Ok(())
}
