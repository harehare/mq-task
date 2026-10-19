//! mq_task - Markdown-based task runner CLI

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{ArgValueCompleter, CompleteEnv, CompletionCandidate};
use colored::*;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::Duration;

use mq_task::{
    Config, Error, ExecutionMode, GraphFormat, INTERRUPTED, OutputMode, Result, RunOptions, Runner,
};

/// Shortest pause between `--watch` polls.
const MIN_WATCH_INTERVAL: Duration = Duration::from_millis(50);

/// The `--interval` pause, raised to the minimum so that `0` cannot make the
/// watcher poll the filesystem without pausing.
fn watch_interval(raw: &str) -> Result<Duration> {
    Ok(mq_task::util::parse_duration(raw)?.max(MIN_WATCH_INTERVAL))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_interval_has_a_floor() {
        assert_eq!(watch_interval("0").unwrap(), MIN_WATCH_INTERVAL);
        assert_eq!(watch_interval("1ms").unwrap(), MIN_WATCH_INTERVAL);
        assert_eq!(watch_interval("2s").unwrap(), Duration::from_secs(2));
        assert!(watch_interval("soon").is_err());
    }
}

/// Flags controlling how a task's dependency graph is executed.
#[derive(Args, Clone, Default)]
struct ExecFlags {
    /// Run dependencies concurrently (overrides each task's `parallel` setting)
    #[arg(long)]
    parallel: bool,

    /// Maximum number of tasks to run at once (1 forces sequential execution)
    #[arg(short = 'j', long, value_name = "N")]
    jobs: Option<usize>,

    /// Keep running independent tasks after one fails
    #[arg(short = 'k', long)]
    keep_going: bool,

    /// Ignore `sources`/`outputs` up-to-date checks and always run tasks
    #[arg(long)]
    no_cache: bool,

    /// Answer yes to every `prompt` without asking
    #[arg(short = 'y', long)]
    yes: bool,

    /// Show each code block before it runs
    #[arg(short = 'v', long)]
    verbose: bool,

    /// How task output is shown: interleaved, prefixed or group
    #[arg(long, value_name = "MODE")]
    output: Option<OutputMode>,
}

/// Flags that only apply when tasks are run from the command line (not in
/// the TUI).
#[derive(Args, Clone, Default)]
struct RunFlags {
    /// Print only task output and errors, not progress lines
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Print how long each task took when the run ends
    #[arg(long)]
    timings: bool,

    /// Run only the named tasks, not their `depends`
    #[arg(long)]
    no_deps: bool,

    /// Report whether the tasks are up to date instead of running them
    /// (exit code 1 when any would run)
    #[arg(long)]
    status: bool,
}

/// Everything needed to run tasks from the command line.
struct RunRequest {
    file: PathBuf,
    tasks: Vec<String>,
    config: Option<PathBuf>,
    runtime: Vec<String>,
    execution_mode: Option<String>,
    lang: Option<String>,
    dry_run: bool,
    env: Vec<String>,
    dir: Option<PathBuf>,
    args: Vec<String>,
    flags: ExecFlags,
    watch: bool,
    interval: String,
    summary: bool,
    run: RunFlags,
}

#[derive(Parser)]
#[command(name = "mq_task")]
#[command(about = "Markdown-based task runner", long_about = None)]
#[command(version)]
struct Cli {
    /// Names of the tasks to execute, in order (shorthand for 'run' command)
    #[arg(value_name = "TASK", num_args = 0.., add = ArgValueCompleter::new(complete_task))]
    task: Vec<String>,

    /// Markdown task file (default: the nearest tasks.md, else README.md)
    #[arg(short, long, env = "MQ_TASK_FILE")]
    file: Option<PathBuf>,

    /// Path to configuration file (default: mq-task.toml next to the task file)
    #[arg(short, long, env = "MQ_TASK_CONFIG")]
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

    /// Re-run the task whenever its `sources` (or the task file) change
    #[arg(short = 'w', long)]
    watch: bool,

    /// How often `--watch` checks for changes (e.g. 500ms, 2s)
    #[arg(long, value_name = "DURATION", default_value = "300ms")]
    interval: String,

    /// Describe the task instead of running it
    #[arg(long)]
    summary: bool,

    #[command(flatten)]
    flags: ExecFlags,

    #[command(flatten)]
    run: RunFlags,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a task from a markdown file
    Run {
        /// Names (section titles) of the tasks to execute, in order
        #[arg(required = true, num_args = 1.., add = ArgValueCompleter::new(complete_task))]
        task: Vec<String>,

        /// Markdown task file (default: the nearest tasks.md, else README.md)
        #[arg(short, long, env = "MQ_TASK_FILE")]
        file: Option<PathBuf>,

        /// Path to configuration file (default: mq-task.toml next to the task file)
        #[arg(short, long, env = "MQ_TASK_CONFIG")]
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

        /// Re-run the task whenever its `sources` (or the task file) change
        #[arg(short = 'w', long)]
        watch: bool,

        /// How often `--watch` checks for changes (e.g. 500ms, 2s)
        #[arg(long, value_name = "DURATION", default_value = "300ms")]
        interval: String,

        /// Describe the task instead of running it
        #[arg(long)]
        summary: bool,

        #[command(flatten)]
        flags: ExecFlags,

        #[command(flatten)]
        run: RunFlags,

        /// Arguments to pass to the task (use -- to separate: mq_task run task -- arg1 arg2)
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// List all available tasks in a markdown file
    List {
        /// Markdown task file (default: the nearest tasks.md, else README.md)
        #[arg(short, long, env = "MQ_TASK_FILE")]
        file: Option<PathBuf>,

        /// Path to configuration file (default: mq-task.toml next to the task file)
        #[arg(short, long, env = "MQ_TASK_CONFIG")]
        config: Option<PathBuf>,

        /// Filter code blocks by language (e.g., bash, python, go)
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,

        /// Include private tasks (name starts with `_` or `meta` has private = true)
        #[arg(short, long)]
        all: bool,

        /// Print the tasks as JSON instead of text
        #[arg(long)]
        json: bool,
    },

    /// Interactively select and run a task using TUI
    Tui {
        /// Markdown task file (default: the nearest tasks.md, else README.md)
        #[arg(short, long, env = "MQ_TASK_FILE")]
        file: Option<PathBuf>,

        /// Path to configuration file (default: mq-task.toml next to the task file)
        #[arg(short, long, env = "MQ_TASK_CONFIG")]
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

        #[command(flatten)]
        flags: ExecFlags,
    },

    /// Check the task file for mistakes without running anything
    Check {
        /// Markdown task file (default: the nearest tasks.md, else README.md)
        #[arg(short, long, env = "MQ_TASK_FILE")]
        file: Option<PathBuf>,

        /// Path to configuration file (default: mq-task.toml next to the task file)
        #[arg(short, long, env = "MQ_TASK_CONFIG")]
        config: Option<PathBuf>,

        /// Fail on warnings too
        #[arg(long)]
        strict: bool,

        /// Print the result as JSON
        #[arg(long)]
        json: bool,
    },

    /// Print the dependency graph of a task (or of all tasks)
    Graph {
        /// Task to show the dependencies of; all public tasks when omitted
        #[arg(add = ArgValueCompleter::new(complete_task))]
        task: Option<String>,

        /// Markdown task file (default: the nearest tasks.md, else README.md)
        #[arg(short, long, env = "MQ_TASK_FILE")]
        file: Option<PathBuf>,

        /// Path to configuration file (default: mq-task.toml next to the task file)
        #[arg(short, long, env = "MQ_TASK_CONFIG")]
        config: Option<PathBuf>,

        /// Output format: text or mermaid
        #[arg(long, default_value = "text", value_name = "FORMAT")]
        format: GraphFormat,
    },

    /// Print the shell completion script (completes options and task names)
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },

    /// Create a starter task file (or, with --config, a configuration file)
    Init {
        /// Where to write it (default: tasks.md, or mq-task.toml with --config)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Write a sample configuration file instead of a task file
        #[arg(long)]
        config: bool,
    },
}

fn main() -> ExitCode {
    // Avoid dying by signal on Ctrl+C; some shells (e.g. nushell) treat
    // that as a hard error and trip wrapper tooling like npm/pnpm.
    // The flag lets long-running loops such as `--watch` shut down.
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));

    // Answers shell completion requests (`COMPLETE=<shell> mq-task ...`).
    CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();

    match dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `check` and `--status` have already printed what is wrong.
            if !matches!(err, Error::CheckFailed | Error::OutOfDate(_)) {
                eprintln!("{} {}", "Error:".red().bold(), err);
                if std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true") {
                    println!(
                        "{}",
                        mq_task::util::github_error("mq-task", &err.to_string())
                    );
                }
            }
            ExitCode::from(err.exit_code().clamp(0, 255) as u8)
        }
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Commands::Run {
            file,
            task,
            mut config,
            runtime,
            execution_mode,
            lang,
            dry_run,
            env,
            mut dir,
            watch,
            interval,
            summary,
            flags,
            run,
            args,
        }) => {
            let file = locate_task_file(file, &mut [&mut config, &mut dir], run.quiet)?;
            run_task(RunRequest {
                file,
                tasks: task,
                config,
                runtime,
                execution_mode,
                lang,
                dry_run,
                env,
                dir,
                args,
                flags,
                watch,
                interval,
                summary,
                run,
            })?
        }
        Some(Commands::List {
            file,
            mut config,
            lang,
            all,
            json,
        }) => {
            let file = locate_task_file(file, &mut [&mut config], false)?;
            list_tasks(file, config, lang, all, json)?
        }
        Some(Commands::Tui {
            file,
            mut config,
            lang,
            dry_run,
            env,
            mut dir,
            all,
            flags,
        }) => {
            let file = locate_task_file(file, &mut [&mut config, &mut dir], false)?;
            run_tui(file, config, lang, dry_run, env, dir, all, flags)?
        }
        Some(Commands::Check {
            file,
            mut config,
            strict,
            json,
        }) => {
            let file = locate_task_file(file, &mut [&mut config], false)?;
            check_tasks(&file, config, strict, json)?
        }
        Some(Commands::Graph {
            task,
            file,
            mut config,
            format,
        }) => {
            let file = locate_task_file(file, &mut [&mut config], false)?;
            show_graph(file, config, task, format)?
        }
        Some(Commands::Completions { shell }) => print_completion_script(shell)?,
        Some(Commands::Init { output, config }) => init(output, config)?,
        None => {
            let (mut config, mut dir) = (cli.config, cli.dir);
            let file = locate_task_file(cli.file, &mut [&mut config, &mut dir], cli.run.quiet)?;
            // No task given: fall back to the configured default task, if any.
            let tasks = if cli.task.is_empty() {
                default_task(config.clone(), &file)?.into_iter().collect()
            } else {
                cli.task
            };
            match tasks {
                tasks if !tasks.is_empty() => run_task(RunRequest {
                    file,
                    tasks,
                    config,
                    runtime: cli.runtime,
                    execution_mode: cli.execution_mode,
                    lang: cli.lang,
                    dry_run: cli.dry_run,
                    env: cli.env,
                    dir,
                    args: cli.args,
                    flags: cli.flags,
                    watch: cli.watch,
                    interval: cli.interval,
                    summary: cli.summary,
                    run: cli.run,
                })?,
                _ => list_tasks(file, config, cli.lang, cli.all, false)?,
            }
        }
    }

    Ok(())
}

/// Build a runner from the CLI settings shared by `run` and `tui`.
#[allow(clippy::too_many_arguments)]
fn build_runner(
    config_path: Option<PathBuf>,
    task_file: &Path,
    runtime_overrides: &[String],
    execution_mode: Option<&str>,
    dry_run: bool,
    env: &[String],
    dir: Option<PathBuf>,
    flags: &ExecFlags,
) -> Result<Runner> {
    let mut config = load_config(config_path.clone(), task_file)?;
    let mut pinned = explicit_languages(config_path.as_deref(), task_file);

    let exec_mode = execution_mode.map(ExecutionMode::try_from).transpose()?;
    if !runtime_overrides.is_empty() {
        config.apply_runtime_overrides(runtime_overrides, exec_mode)?;
    }

    pinned.extend(
        runtime_overrides
            .iter()
            .filter_map(|o| o.split_once(':').map(|(lang, _)| lang.to_string())),
    );
    let mut runner = Runner::new(config);
    runner.pin_runtimes(pinned);
    runner.set_dry_run(dry_run);
    runner.set_env_overrides(Runner::parse_env_overrides(env)?);
    runner.set_working_dir(dir);
    runner.set_use_cache(!flags.no_cache);
    runner.set_assume_yes(flags.yes);
    runner.set_verbose(flags.verbose);
    Ok(runner)
}

/// Task and alias names of the task file, for shell completion. The file is
/// found the same way as for a run: `MQ_TASK_FILE`, else a search from the
/// current directory. Anything that goes wrong simply yields no candidates.
fn complete_task(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Some(prefix) = current.to_str() else {
        return Vec::new();
    };
    let Some(file) = std::env::var_os("MQ_TASK_FILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|dir| mq_task::discover::find_task_file(&dir))
        })
    else {
        return Vec::new();
    };
    let Ok(sections) = Runner::with_default_config().list_task_sections(&file) else {
        return Vec::new();
    };
    sections
        .iter()
        .filter(|s| !s.private)
        .flat_map(|s| {
            let help = s
                .description
                .as_deref()
                .and_then(|d| d.lines().next())
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty());
            std::iter::once(s.title.as_str())
                .chain(s.aliases.iter().map(String::as_str))
                .map(move |name| CompletionCandidate::new(name).help(help.clone().map(Into::into)))
                .collect::<Vec<_>>()
        })
        .filter(|c| c.get_value().to_string_lossy().starts_with(prefix))
        .collect()
}

/// Print the registration script that makes the shell ask mq-task for
/// completions (see [`complete_task`]).
fn print_completion_script(shell: clap_complete::Shell) -> Result<()> {
    let name = shell.to_string();
    let shells = clap_complete::env::Shells::builtins();
    let completer = shells
        .completer(&name)
        .ok_or_else(|| Error::Config(format!("no completions for shell '{}'", name)))?;
    completer.write_registration(
        "COMPLETE",
        "mq-task",
        "mq-task",
        "mq-task",
        &mut std::io::stdout(),
    )?;
    Ok(())
}

/// Add a "did you mean" hint to an unknown-task error, using the task and
/// alias names of `file`.
fn with_task_hint(err: Error, runner: &mut Runner, file: &Path) -> Error {
    let Error::SectionNotFound(name) = &err else {
        return err;
    };
    let Ok(sections) = runner.list_task_sections(file) else {
        return err;
    };
    let names = || {
        sections.iter().flat_map(|s| {
            std::iter::once(s.title.as_str()).chain(s.aliases.iter().map(String::as_str))
        })
    };
    // A dependency call such as `deploi env=prod` is looked up by its first word.
    let first_word = name.split_whitespace().next().unwrap_or(name);
    match mq_task::util::closest(name, names())
        .or_else(|| mq_task::util::closest(first_word, names()))
    {
        Some(suggestion) => {
            Error::SectionNotFound(format!("{} (did you mean `{}`?)", name, suggestion))
        }
        None => err,
    }
}

/// Run the requested tasks, once or (with `--watch`) whenever their inputs change
fn run_task(req: RunRequest) -> Result<()> {
    let mut runner = build_runner(
        req.config,
        &req.file,
        &req.runtime,
        req.execution_mode.as_deref(),
        req.dry_run,
        &req.env,
        req.dir,
        &req.flags,
    )?;

    if req.summary {
        for (i, task) in req.tasks.iter().enumerate() {
            let text = runner
                .task_summary(&req.file, task)
                .map_err(|e| with_task_hint(e, &mut runner, &req.file))?;
            if i > 0 {
                println!();
            }
            print!("{}", text);
        }
        return Ok(());
    }

    let options = || RunOptions {
        args: req.args.clone(),
        lang_filter: req.lang.clone(),
        parallel: req.flags.parallel,
        jobs: req.flags.jobs,
        keep_going: req.flags.keep_going,
        output: req.flags.output.unwrap_or_default(),
        observer: None,
        cancel: None,
        quiet: req.run.quiet,
        timings: req.run.timings,
        no_deps: req.run.no_deps,
    };

    if req.run.status {
        return report_status(
            &mut runner,
            &req.file,
            &req.tasks,
            &options(),
            req.run.quiet,
        );
    }

    if !req.watch {
        return runner
            .run_tasks_with_options(&req.file, &req.tasks, options())
            .map_err(|e| with_task_hint(e, &mut runner, &req.file));
    }
    let interval = watch_interval(&req.interval)?;

    loop {
        if let Err(err) = runner.run_tasks_with_options(&req.file, &req.tasks, options()) {
            let err = with_task_hint(err, &mut runner, &req.file);
            eprintln!("{} {}", "Error:".red().bold(), err);
        }
        if INTERRUPTED.load(Ordering::SeqCst) {
            return Ok(());
        }

        let targets = runner.watch_targets_for(&req.file, &req.tasks, req.run.no_deps)?;
        let snapshot = || {
            let mut all = std::collections::HashMap::new();
            for target in &targets {
                all.extend(target.snapshot());
            }
            all
        };
        let before = snapshot();
        println!(
            "{}",
            format!("watching {} file(s), Ctrl+C to stop", before.len()).dimmed()
        );

        loop {
            std::thread::sleep(interval);
            if INTERRUPTED.load(Ordering::SeqCst) {
                return Ok(());
            }
            if snapshot() != before {
                // Let editors finish writing before re-running.
                std::thread::sleep(Duration::from_millis(100));
                break;
            }
        }
    }
}

/// `--status`: print whether each task would run, and fail if any would.
fn report_status(
    runner: &mut Runner,
    file: &Path,
    tasks: &[String],
    options: &RunOptions,
    quiet: bool,
) -> Result<()> {
    let states = runner
        .task_states(file, tasks, options)
        .map_err(|e| with_task_hint(e, runner, file))?;
    for state in &states {
        if state.up_to_date {
            if !quiet {
                println!(
                    "{} {} {}",
                    "✔".green().bold(),
                    state.task.bold(),
                    format!("({})", state.detail).dimmed()
                );
            }
        } else {
            println!(
                "{} {} {}",
                "✘".red().bold(),
                state.task.bold(),
                state.detail
            );
        }
    }
    match states.iter().find(|s| !s.up_to_date) {
        Some(state) => Err(Error::OutOfDate(state.task.clone())),
        None => Ok(()),
    }
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
    flags: ExecFlags,
) -> Result<()> {
    let config = load_config(config_path, &markdown_path)?;
    mq_task::tui::run_tui(
        markdown_path,
        config,
        mq_task::tui::TuiOptions {
            lang_filter,
            dry_run,
            env,
            dir,
            show_all,
            parallel: flags.parallel,
            jobs: flags.jobs,
            keep_going: flags.keep_going,
            no_cache: flags.no_cache,
            yes: flags.yes,
            verbose: flags.verbose,
        },
    )?;
    Ok(())
}

/// Check the task file and print what is wrong with it.
fn check_tasks(file: &Path, config: Option<PathBuf>, strict: bool, json: bool) -> Result<()> {
    let mut runner = runner_for(config, file)?;
    let command = Cli::command();
    let reserved: Vec<String> = command
        .get_subcommands()
        .flat_map(|c| {
            std::iter::once(c.get_name().to_string()).chain(c.get_all_aliases().map(str::to_string))
        })
        .chain(std::iter::once("help".to_string()))
        .collect();
    let report = mq_task::check::check_file(&mut runner, file, &reserved);

    if json {
        let value = serde_json::json!({
            "file": file.display().to_string(),
            "tasks": report.tasks,
            "diagnostics": report.diagnostics,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        print!("{}", format_report(file, &report));
    }
    if report.passes(strict) {
        Ok(())
    } else {
        Err(Error::CheckFailed)
    }
}

/// The text report of `mq-task check`.
fn format_report(file: &Path, report: &mq_task::check::Report) -> String {
    use mq_task::check::Severity;
    let mut out = String::new();
    for d in &report.diagnostics {
        let label = match d.severity {
            Severity::Error => "error".red().bold(),
            Severity::Warning => "warning".yellow().bold(),
        };
        match &d.task {
            Some(task) => out.push_str(&format!("{}: task '{}': {}\n", label, task, d.message)),
            None => out.push_str(&format!("{}: {}\n", label, d.message)),
        }
    }
    let (errors, warnings) = (report.errors(), report.warnings());
    if report.diagnostics.is_empty() {
        out.push_str(&format!(
            "{} {}: {} tasks, no problems found\n",
            "✔".green().bold(),
            file.display(),
            report.tasks
        ));
    } else {
        let plural =
            |n: usize, word: &str| format!("{} {}{}", n, word, if n == 1 { "" } else { "s" });
        out.push_str(&format!(
            "\n{}: {}, {}\n",
            file.display(),
            plural(errors, "error"),
            plural(warnings, "warning")
        ));
    }
    out
}

/// Print the dependency graph
fn show_graph(
    markdown_path: PathBuf,
    config_path: Option<PathBuf>,
    task: Option<String>,
    format: GraphFormat,
) -> Result<()> {
    let mut runner = runner_for(config_path, &markdown_path)?;
    print!(
        "{}",
        runner.dependency_graph(&markdown_path, task.as_deref(), format)?
    );
    Ok(())
}

/// List all available tasks
fn list_tasks(
    markdown_path: PathBuf,
    config_path: Option<PathBuf>,
    lang_filter: Option<String>,
    show_all: bool,
    json: bool,
) -> Result<()> {
    let mut runner = runner_for(config_path, &markdown_path)?;

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

    if json {
        println!("{}", mq_task::summary::list_json(&filtered_sections));
        return Ok(());
    }

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

/// Write a starter task file, or a configuration file with `--config`.
/// Existing files are never overwritten.
fn init(output: Option<PathBuf>, config: bool) -> Result<()> {
    let (default_name, kind) = if config {
        (mq_task::config::CONFIG_FILE, "Configuration file")
    } else {
        ("tasks.md", "Task file")
    };
    let path = output.unwrap_or_else(|| PathBuf::from(default_name));

    let content = if config {
        toml::to_string_pretty(&Config::default())
            .map_err(|e| Error::Config(format!("Failed to serialize configuration: {}", e)))?
    } else {
        mq_task::template::TASKS_TEMPLATE.to_string()
    };
    // `create_new` makes the existence check and the creation one atomic step,
    // so a file created concurrently is never truncated.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => {
                Error::Config(format!("{} already exists: {}", kind, path.display()))
            }
            _ => Error::from(e),
        })?;
    std::io::Write::write_all(&mut file, content.as_bytes())?;
    println!("{} created: {}", kind, path.display());
    if !config {
        println!(
            "{}",
            "Try `mq-task` to list the tasks and `mq-task hello` to run one.".dimmed()
        );
    }
    Ok(())
}

/// Load the configuration: `-c`, else `mq-task.toml` next to the task file,
/// else the defaults.
/// The task to run when none is named: `default_task` of the configuration
/// file, else of the task file's own `meta` block.
fn default_task(config_path: Option<PathBuf>, task_file: &Path) -> Result<Option<String>> {
    if let Some(task) = load_config(config_path.clone(), task_file)?.default_task {
        return Ok(Some(task));
    }
    let mut runner = runner_for(config_path, task_file)?;
    runner.load_sections(task_file)?;
    Ok(runner.document_default_task().map(str::to_string))
}

/// The languages the configuration file (given, or beside the task file)
/// sets a runtime for; the task file's own `runtimes` do not replace them.
fn explicit_languages(config_path: Option<&Path>, task_file: &Path) -> Vec<String> {
    match config_path {
        Some(path) => Config::explicit_languages(path),
        None => Config::explicit_languages(Config::path_beside(task_file)),
    }
}

/// A runner for reading the task file, with the configuration pinned like
/// `build_runner` does.
fn runner_for(config_path: Option<PathBuf>, task_file: &Path) -> Result<Runner> {
    let config = load_config(config_path.clone(), task_file)?;
    let mut runner = Runner::new(config);
    runner.pin_runtimes(explicit_languages(config_path.as_deref(), task_file));
    Ok(runner)
}

fn load_config(config_path: Option<PathBuf>, task_file: &Path) -> Result<Config> {
    match config_path {
        Some(path) => Config::from_file(path),
        None => Config::discover(task_file),
    }
}

/// Find the task file. Without `-f` it is searched for (see
/// [`mq_task::discover`]); one found in a parent directory makes mq-task
/// switch to that directory first, like `just`. The paths in `relative` are
/// made absolute beforehand so they keep pointing where the user meant.
fn locate_task_file(
    file: Option<PathBuf>,
    relative: &mut [&mut Option<PathBuf>],
    quiet: bool,
) -> Result<PathBuf> {
    if let Some(file) = file {
        return Ok(file);
    }
    let cwd = std::env::current_dir()?;
    let found = mq_task::discover::find_task_file(&cwd)
        .ok_or_else(|| Error::NoTaskFile(cwd.display().to_string()))?;
    let (dir, name) = match (found.parent(), found.file_name()) {
        (Some(dir), Some(name)) => (dir.to_path_buf(), PathBuf::from(name)),
        _ => return Ok(found),
    };
    if dir != cwd {
        for path in relative.iter_mut() {
            if let Some(p) = path.as_mut()
                && p.is_relative()
            {
                *p = cwd.join(&*p);
            }
        }
        std::env::set_current_dir(&dir)?;
        if !quiet {
            eprintln!("{}", format!("using {}", found.display()).dimmed());
        }
    }
    Ok(name)
}
