//! Core library for tmz - Microsoft Teams CLI and TUI.
//!
//! This crate provides:
//! - Configuration loading and management
//! - XDG-compliant path resolution
//! - Schema and example config generation
//! - Teams API client and authentication
//! - `SQLite` cache for offline search and fast access
//! - Common types and error handling

pub mod cache;
pub mod config;
pub mod daemon;
pub mod error;
pub mod paths;
pub mod schema;
pub mod teams;

pub use cache::{Cache, CachedConversation, CachedMessage, SearchResult};
pub use config::{AppConfig, LogLevel, LoggingConfig, PathsConfig, RuntimeConfig};
pub use error::{CoreError, Result};
pub use paths::{AppPaths, default_cache_dir, default_data_dir, default_state_dir};
pub use schema::{generate_example_config, generate_schema, write_generated_files};
pub use teams::{AuthManager, TeamsClient, TeamsTokens};

/// Application name used for config directories and environment prefix.
pub const APP_NAME: &str = "tmz";

/// Returns the environment variable prefix for this application.
#[must_use]
pub fn env_prefix() -> String {
    APP_NAME
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Returns the default parallelism based on available CPU cores.
#[must_use]
pub fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
}
