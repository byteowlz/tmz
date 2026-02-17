//! Error types for the core library.

use thiserror::Error;

/// Core library error type.
#[derive(Debug, Error)]
pub enum CoreError {
    /// A configuration-related error.
    #[error("configuration error: {0}")]
    Config(String),

    /// A path resolution or validation error.
    #[error("path error: {0}")]
    Path(String),

    /// An I/O error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// A serialization or deserialization error.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// An authentication or secret-related error.
    #[error("authentication error: {0}")]
    Auth(String),

    /// A secret not found error.
    #[error("secret not found: {0}")]
    SecretNotFound(String),

    /// An API or HTTP error.
    #[error("API error: {0}")]
    Api(String),

    /// A generic error for other cases.
    #[error("error: {0}")]
    Other(String),
}

/// Result type alias using `CoreError`.
pub type Result<T> = std::result::Result<T, CoreError>;
