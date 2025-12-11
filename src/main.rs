//! mq_task - Markdown-based task runner CLI

use clap::{Parser, Subcommand};
use colored::*;
use miette::{IntoDiagnostic, Result};
use std::path::PathBuf;

use mq_task::{Config, ExecutionMode, Runner};

const DEFAULT_TASKS_FILE: &str = "README.md";

#[derive(Parser)]
#[command(name = "mq_task")]
#[command(about = "Markdown-based task runner", long_about = None)]
#[command(version)]
struct Cli {
    /// Task name to execute (shorthand for 'run' command)
    #[arg(value_name = "TASK")]
    task: Option<String>,

    /// Path to the markdown file
    #[arg(short, long, default_value = DEFAULT_TASKS_FILE)]
    file: PathBuf,

    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Heading level for sections (1-6)
    #[arg(short, long)]
    level: Option<u8>,

    /// Override runtime for a language (format: lang:command, e.g., python:python3.11)
    #[arg(short, long, value_name = "LANG:COMMAND")]
    runtime: Vec<String>,

    /// Set execution mode for runtime overrides (stdin, file, arg)
    #[arg(short, long, value_name = "MODE")]
    execution_mode: Option<String>,

    /// Filter code blocks by language (e.g., bash, python, go)
    #[arg(long, value_name = "LANG")]
    lang: Option<String>,

    /// Arguments to pass to the task (use -- to separate: mq_task task -- arg1 arg2)
    #[arg(last = true)]
    args: Vec<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a task from a markdown file
    Run {
        /// Task name (section title) to execute
        task: String,

        /// Path to the markdown file
        #[arg(short, long, default_value = DEFAULT_TASKS_FILE)]
        file: PathBuf,

        /// Path to configuration file
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Heading level for sections (1-6)
        #[arg(short, long)]
        level: Option<u8>,

        /// Override runtime for a language (format: lang:command, e.g., python:python3.11)
        #[arg(short, long, value_name = "LANG:COMMAND")]
        runtime: Vec<String>,

        /// Set execution mode for runtime overrides (stdin, file, arg)
        #[arg(short, long, value_name = "MODE")]
        execution_mode: Option<String>,

        /// Filter code blocks by language (e.g., bash, python, go)
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,

        /// Arguments to pass to the task (use -- to separate: mq_task run task -- arg1 arg2)
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// List all available tasks in a markdown file
    List {
        /// Path to the markdown file
        #[arg(short, long, default_value = DEFAULT_TASKS_FILE)]
        file: PathBuf,

        /// Path to configuration file
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Heading level for sections (1-6)
        #[arg(short, long)]
        level: Option<u8>,

        /// Filter code blocks by language (e.g., bash, python, go)
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,
    },

    /// Generate a sample configuration file
    Init {
        /// Output path for configuration file
        #[arg(short, long, default_value = "mq_task.toml")]
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run {
            file,
            task,
            config,
            level,
            runtime,
            execution_mode,
            lang,
            args,
        }) => run_task(
            file,
            task,
            config,
            level,
            runtime,
            execution_mode,
            lang,
            args,
        )?,
        Some(Commands::List {
            file,
            config,
            level,
            lang,
        }) => list_tasks(file, config, level, lang)?,
        Some(Commands::Init { output }) => init_config(output)?,
        None => {
            // If no subcommand, check if task is provided
            if let Some(task) = cli.task {
                run_task(
                    cli.file,
                    task,
                    cli.config,
                    cli.level,
                    cli.runtime,
                    cli.execution_mode,
                    cli.lang,
                    cli.args,
                )?;
            } else {
                // No task provided, list available tasks
                list_tasks(cli.file, cli.config, cli.level, cli.lang)?;
            }
        }
    }

    Ok(())
}

/// Run a specific task
#[allow(clippy::too_many_arguments)]
fn run_task(
    markdown_path: PathBuf,
    task_name: String,
    config_path: Option<PathBuf>,
    level: Option<u8>,
    runtime_overrides: Vec<String>,
    execution_mode: Option<String>,
    lang_filter: Option<String>,
    args: Vec<String>,
) -> Result<()> {
    let mut config = load_config(config_path)?;

    // Override heading level if specified
    if let Some(level) = level {
        config.heading_level = level;
    }

    // Parse execution mode if specified
    let exec_mode = if let Some(mode_str) = execution_mode {
        Some(ExecutionMode::try_from(mode_str.as_str()).into_diagnostic()?)
    } else {
        None
    };

    // Apply runtime overrides
    if !runtime_overrides.is_empty() {
        config
            .apply_runtime_overrides(&runtime_overrides, exec_mode)
            .into_diagnostic()?;
    }

    let mut runner = Runner::new(config);

    println!("Running task: {}", task_name);
    println!();

    runner
        .run_task_with_lang_filter(&markdown_path, &task_name, &args, lang_filter.as_deref())
        .into_diagnostic()?;

    Ok(())
}

/// List all available tasks
fn list_tasks(
    markdown_path: PathBuf,
    config_path: Option<PathBuf>,
    level: Option<u8>,
    lang_filter: Option<String>,
) -> Result<()> {
    let mut config = load_config(config_path)?;

    // Override heading level if specified
    if let Some(level) = level {
        config.heading_level = level;
    }

    let mut runner = Runner::new(config);

    let sections = runner
        .list_task_sections(&markdown_path)
        .into_diagnostic()?;

    // Filter sections by language if specified
    let filtered_sections: Vec<_> = if let Some(ref lang) = lang_filter {
        sections
            .into_iter()
            .filter(|section| section.codes.iter().any(|code| code.lang == *lang))
            .collect()
    } else {
        sections
    };

    if filtered_sections.is_empty() {
        if lang_filter.is_some() {
            println!(
                "{}",
                format!(
                    "No tasks found with language '{}' in {}",
                    lang_filter.unwrap(),
                    markdown_path.display()
                )
                .yellow()
            );
        } else {
            println!(
                "{}",
                format!("No tasks found in {}", markdown_path.display()).yellow()
            );
        }
        return Ok(());
    }

    let mut output = String::new();
    output.push_str(&format!(
        "{} {}{}\n\n",
        "Available tasks in".bold(),
        markdown_path.display().to_string().cyan(),
        if let Some(ref lang) = lang_filter {
            format!(" {}", format!("(language: {})", lang).bright_black())
        } else {
            String::new()
        }
    ));

    for section in filtered_sections {
        // Show language information if filtering is active
        let lang_info = if lang_filter.is_some() {
            let langs: Vec<String> = section
                .codes
                .iter()
                .filter_map(|code| {
                    if let Some(ref filter) = lang_filter {
                        if code.lang == *filter {
                            Some(code.lang.clone())
                        } else {
                            None
                        }
                    } else {
                        Some(code.lang.clone())
                    }
                })
                .collect();

            if !langs.is_empty() {
                format!(" {}", format!("[{}]", langs.join(", ")).bright_black())
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if let Some(desc) = section.description {
            let trimmed = desc.trim();
            if !trimmed.is_empty() {
                output.push_str(&format!(
                    "  {}{} {}\n",
                    section.title.green().bold(),
                    lang_info,
                    format!("- {}", trimmed).bright_black()
                ));
            } else {
                output.push_str(&format!(
                    "  {}{}\n",
                    section.title.green().bold(),
                    lang_info
                ));
            }
        } else {
            output.push_str(&format!(
                "  {}{}\n",
                section.title.green().bold(),
                lang_info
            ));
        }
    }

    print!("{}", output);

    Ok(())
}

/// Initialize configuration file
fn init_config(output_path: PathBuf) -> Result<()> {
    if output_path.exists() {
        return Err(miette::miette!(
            "Configuration file already exists: {}",
            output_path.display()
        ));
    }

    let config = Config::default();
    let toml = toml::to_string_pretty(&config).into_diagnostic()?;

    std::fs::write(&output_path, toml).into_diagnostic()?;
    println!("Configuration file created: {}", output_path.display());

    Ok(())
}

/// Load configuration from file or use default
fn load_config(config_path: Option<PathBuf>) -> Result<Config> {
    let config = if let Some(path) = config_path {
        Config::from_file(&path).into_diagnostic()?
    } else {
        Config::default()
    };

    Ok(config)
}
