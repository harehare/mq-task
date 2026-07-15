//! mq_task - Markdown-based task runner CLI

use clap::{Parser, Subcommand};
use colored::*;
use std::path::PathBuf;
use std::process::ExitCode;

use mq_task::{Config, Error, ExecutionMode, Result, Runner};

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

    /// Override runtime for a language (format: lang:command, e.g., python:python3.11)
    #[arg(short, long, value_name = "LANG:COMMAND")]
    runtime: Vec<String>,

    /// Set execution mode for runtime overrides (stdin, file, arg)
    #[arg(short, long, value_name = "MODE")]
    execution_mode: Option<String>,

    /// Filter code blocks by language (e.g., bash, python, go)
    #[arg(long, value_name = "LANG")]
    lang: Option<String>,

    /// Show what would be executed without actually running it
    #[arg(long)]
    dry_run: bool,

    /// Set an environment variable for the task (format: KEY=VALUE), repeatable
    #[arg(long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,

    /// Working directory to run the task's commands in
    #[arg(short = 'd', long = "dir", value_name = "PATH")]
    dir: Option<PathBuf>,

    /// Arguments to pass to the task (use -- to separate: mq_task task -- arg1 arg2)
    #[arg(last = true)]
    args: Vec<String>,

    /// Include private tasks (name starts with `_` or `meta` has private = true) when listing
    #[arg(short, long)]
    all: bool,

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

        /// Override runtime for a language (format: lang:command, e.g., python:python3.11)
        #[arg(short, long, value_name = "LANG:COMMAND")]
        runtime: Vec<String>,

        /// Set execution mode for runtime overrides (stdin, file, arg)
        #[arg(short, long, value_name = "MODE")]
        execution_mode: Option<String>,

        /// Filter code blocks by language (e.g., bash, python, go)
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,

        /// Show what would be executed without actually running it
        #[arg(long)]
        dry_run: bool,

        /// Set an environment variable for the task (format: KEY=VALUE), repeatable
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,

        /// Working directory to run the task's commands in
        #[arg(short = 'd', long = "dir", value_name = "PATH")]
        dir: Option<PathBuf>,

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

        /// Filter code blocks by language (e.g., bash, python, go)
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,

        /// Include private tasks (name starts with `_` or `meta` has private = true)
        #[arg(short, long)]
        all: bool,
    },

    /// Interactively select and run a task using TUI
    Tui {
        /// Path to the markdown file
        #[arg(short, long, default_value = DEFAULT_TASKS_FILE)]
        file: PathBuf,

        /// Path to configuration file
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Filter code blocks by language (e.g., bash, python, go)
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,

        /// Show what would be executed without actually running it
        #[arg(long)]
        dry_run: bool,

        /// Set an environment variable for the task (format: KEY=VALUE), repeatable
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,

        /// Working directory to run the task's commands in
        #[arg(short = 'd', long = "dir", value_name = "PATH")]
        dir: Option<PathBuf>,

        /// Include private tasks (name starts with `_` or `meta` has private = true)
        #[arg(short, long)]
        all: bool,
    },

    /// Generate a sample configuration file
    Init {
        /// Output path for configuration file
        #[arg(short, long, default_value = "mq_task.toml")]
        output: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let code = match &err {
                Error::ExecutionFailed(code) => *code,
                _ => 1,
            };
            eprintln!("{} {}", "Error:".red().bold(), err);
            ExitCode::from(code.clamp(0, 255) as u8)
        }
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Commands::Run {
            file,
            task,
            config,
            runtime,
            execution_mode,
            lang,
            dry_run,
            env,
            dir,
            args,
        }) => run_task(
            file,
            task,
            config,
            runtime,
            execution_mode,
            lang,
            dry_run,
            env,
            dir,
            args,
        )?,
        Some(Commands::List {
            file,
            config,
            lang,
            all,
        }) => list_tasks(file, config, lang, all)?,
        Some(Commands::Tui {
            file,
            config,
            lang,
            dry_run,
            env,
            dir,
            all,
        }) => run_tui(file, config, lang, dry_run, env, dir, all)?,
        Some(Commands::Init { output }) => init_config(output)?,
        None => {
            // If no subcommand, check if task is provided
            if let Some(task) = cli.task {
                run_task(
                    cli.file,
                    task,
                    cli.config,
                    cli.runtime,
                    cli.execution_mode,
                    cli.lang,
                    cli.dry_run,
                    cli.env,
                    cli.dir,
                    cli.args,
                )?;
            } else {
                // No task given: fall back to the configured default task, if any
                let config = load_config(cli.config.clone())?;
                if let Some(default_task) = config.default_task.clone() {
                    run_task(
                        cli.file,
                        default_task,
                        cli.config,
                        cli.runtime,
                        cli.execution_mode,
                        cli.lang,
                        cli.dry_run,
                        cli.env,
                        cli.dir,
                        cli.args,
                    )?;
                } else {
                    list_tasks(cli.file, cli.config, cli.lang, cli.all)?;
                }
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
    runtime_overrides: Vec<String>,
    execution_mode: Option<String>,
    lang_filter: Option<String>,
    dry_run: bool,
    env: Vec<String>,
    dir: Option<PathBuf>,
    args: Vec<String>,
) -> Result<()> {
    let mut config = load_config(config_path)?;

    // Parse execution mode if specified
    let exec_mode = if let Some(mode_str) = execution_mode {
        Some(ExecutionMode::try_from(mode_str.as_str())?)
    } else {
        None
    };

    // Apply runtime overrides
    if !runtime_overrides.is_empty() {
        config.apply_runtime_overrides(&runtime_overrides, exec_mode)?;
    }

    let mut runner = Runner::new(config);
    runner.set_dry_run(dry_run);
    runner.set_env_overrides(Runner::parse_env_overrides(&env)?);
    runner.set_working_dir(dir);

    println!("Running task: {}\n", task_name);

    runner.run_task_with_lang_filter(&markdown_path, &task_name, &args, lang_filter.as_deref())?;

    Ok(())
}

/// Launch the interactive TUI for task selection
#[allow(clippy::too_many_arguments)]
fn run_tui(
    markdown_path: PathBuf,
    config_path: Option<PathBuf>,
    lang_filter: Option<String>,
    dry_run: bool,
    env: Vec<String>,
    dir: Option<PathBuf>,
    show_all: bool,
) -> Result<()> {
    let config = load_config(config_path)?;
    mq_task::tui::run_tui(
        markdown_path,
        config,
        lang_filter,
        dry_run,
        env,
        dir,
        show_all,
    )?;
    Ok(())
}

/// List all available tasks
fn list_tasks(
    markdown_path: PathBuf,
    config_path: Option<PathBuf>,
    lang_filter: Option<String>,
    show_all: bool,
) -> Result<()> {
    let config = load_config(config_path)?;
    let mut runner = Runner::new(config);

    let sections = runner.list_task_sections(&markdown_path)?;

    // Filter sections by language if specified, and hide private tasks unless --all
    let filtered_sections: Vec<_> = sections
        .into_iter()
        .filter(|section| show_all || !section.private)
        .filter(|section| {
            lang_filter
                .as_ref()
                .is_none_or(|lang| section.codes.iter().any(|code| code.lang == *lang))
        })
        .collect();

    if filtered_sections.is_empty() {
        if let Some(ref lang) = lang_filter {
            println!(
                "{}",
                format!(
                    "No tasks found with language '{}' in {}",
                    lang,
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

        let title_display = if section.aliases.is_empty() {
            section.title.green().bold().to_string()
        } else {
            format!(
                "{} {}",
                section.title.green().bold(),
                format!("({})", section.aliases.join(", ")).bright_black()
            )
        };

        if let Some(desc) = section.description {
            let trimmed = desc.trim();
            if !trimmed.is_empty() {
                output.push_str(&format!(
                    "  {}{} {}\n",
                    title_display,
                    lang_info,
                    format!("- {}", trimmed).bright_black()
                ));
            } else {
                output.push_str(&format!("  {}{}\n", title_display, lang_info));
            }
        } else {
            output.push_str(&format!("  {}{}\n", title_display, lang_info));
        }
    }

    print!("{}", output);

    Ok(())
}

/// Initialize configuration file
fn init_config(output_path: PathBuf) -> Result<()> {
    if output_path.exists() {
        return Err(Error::Config(format!(
            "Configuration file already exists: {}",
            output_path.display()
        )));
    }

    let config = Config::default();
    let toml = toml::to_string_pretty(&config)
        .map_err(|e| Error::Config(format!("Failed to serialize configuration: {}", e)))?;

    std::fs::write(&output_path, toml)?;
    println!("Configuration file created: {}", output_path.display());

    Ok(())
}

/// Load configuration from file or use default
fn load_config(config_path: Option<PathBuf>) -> Result<Config> {
    let config = if let Some(path) = config_path {
        Config::from_file(&path)?
    } else {
        Config::default()
    };

    Ok(config)
}
