//! mq_task - Markdown-based task runner
//!
//! mq_task is a task runner that executes code blocks in Markdown files based on section titles.
//! It uses mq query language to parse and extract sections from Markdown documents.

pub mod config;
pub mod error;
pub mod runner;

pub use config::{Config, ExecutionMode};
pub use error::{Error, Result};
pub use runner::Runner;
