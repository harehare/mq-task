//! mq_task - Markdown-based task runner
//!
//! mq_task is a task runner that executes code blocks in Markdown files based on section titles.
//! It uses mq query language to parse and extract sections from Markdown documents.

pub mod cache;
pub mod condition;
pub mod config;
pub mod discover;
pub mod error;
pub mod events;
pub mod exec;
pub mod graph;
pub mod runner;
pub mod schedule;
pub mod summary;
pub mod tui;
pub mod util;

pub use config::{Config, ExecutionMode};
pub use error::{Error, Result};
pub use events::{ConsoleObserver, OutputMode, RunObserver, Stream, TaskStatus};
pub use graph::GraphFormat;
pub use runner::Runner;
pub use runner::{RunOptions, WatchTarget};

use std::sync::atomic::AtomicBool;

/// Set when the user presses Ctrl+C. The process itself keeps running (so
/// child processes can report their own exit status); long-lived loops such
/// as `--watch` check this to shut down.
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);
