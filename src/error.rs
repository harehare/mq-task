//! Error types for mq_task

use thiserror::Error;

/// Result type for mq_task operations
pub type Result<T> = std::result::Result<T, Error>;

/// Error types for mq_task operations
#[derive(Error, Debug)]
pub enum Error {
    /// Error reading or parsing Markdown file
    #[error("Markdown error: {0}")]
    Markdown(String),

    /// Error executing mq query
    #[error("Query error: {0}")]
    Query(String),

    /// Error executing code block
    #[error("Execution error: {0}")]
    Execution(String),

    /// Configuration error
    #[error("Config error: {0}")]
    Config(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML parsing error
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// Section not found
    #[error("Section not found: {0}")]
    SectionNotFound(String),

    /// Runtime not found
    #[error("Runtime not found for language: {0}")]
    RuntimeNotFound(String),
}
