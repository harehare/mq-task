use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use colored::Colorize;
use mq_lang::{DictMap, Engine, Ident, RuntimeValue, parse_markdown_input};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::events::{ConsoleObserver, FailureTracker, OutputMode, RunObserver};
use crate::exec::Executor;
use crate::graph::{self, GraphFormat};
use crate::schedule::{self, PlanContext, ScheduleOptions, build_plan_in, split_words};
use crate::util::parse_duration;
use crate::vars::{self, VarDef};

const SECTIONS_QUERY: &str = include_str!("../sections.mq");

/// Info-string word that keeps a code block from running:
/// ` ```bash no-run `.
const NO_RUN: &str = "no-run";

/// Languages that name data or output rather than something to run. A block
/// in one of them is skipped, unless the configuration gives the language a
/// runtime, so README examples (` ```json `, ` ```text `) do not break a task.
const DATA_LANGUAGES: [&str; 22] = [
    "text",
    "txt",
    "plain",
    "plaintext",
    "console",
    "output",
    "log",
    "json",
    "jsonc",
    "yaml",
    "yml",
    "toml",
    "xml",
    "html",
    "css",
    "csv",
    "diff",
    "ini",
    "markdown",
    "md",
    "mermaid",
    "dockerfile",
];

/// Represents a code block in a section
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeBlock {
    /// Language of the code block
    pub lang: String,
    /// Code content
    pub code: String,
}

/// What a `for` loop iterates over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LoopSpec {
    /// Literal items: `for = ["a", "b"]`
    List(Vec<String>),
    /// Paths matching these globs, relative to the task's directory:
    /// `for = { glob = "src/*.md" }`
    Glob(Vec<String>),
    /// Every combination of the axes' values:
    /// `for = { matrix = { os = ["linux", "macos"], arch = ["x64"] } }`
    /// Axes are kept sorted by name.
    Matrix(Vec<(String, Vec<String>)>),
    /// Lines of a dependency's captured output (`capture = true`):
    /// `for = { from = "list-files" }`. Items run sequentially in the task.
    From(String),
}

impl LoopSpec {
    /// Short description for graph output, e.g. `a, b, c` or `glob src/*.md`.
    pub fn summary(&self) -> String {
        match self {
            LoopSpec::List(items) => {
                let shown: Vec<&str> = items.iter().take(3).map(String::as_str).collect();
                let more = if items.len() > 3 { ", ..." } else { "" };
                format!("{}{}", shown.join(", "), more)
            }
            LoopSpec::Glob(patterns) => format!("glob {}", patterns.join(", ")),
            LoopSpec::From(task) => format!("output of {}", task),
            LoopSpec::Matrix(axes) => format!(
                "matrix {}",
                axes.iter()
                    .map(|(k, _)| k.as_str())
                    .collect::<Vec<_>>()
                    .join(" x ")
            ),
        }
    }

    /// Interpret the TOML value of a `for` key.
    fn from_toml(title: &str, value: &toml::Value) -> Result<Self> {
        let invalid = |msg: &str| Error::Invalid(format!("task '{}': `for`: {}", title, msg));
        let scalar = |v: &toml::Value| -> Result<String> {
            match v {
                toml::Value::String(s) => Ok(s.clone()),
                toml::Value::Integer(i) => Ok(i.to_string()),
                toml::Value::Float(f) => Ok(f.to_string()),
                toml::Value::Boolean(b) => Ok(b.to_string()),
                _ => Err(invalid("items must be strings, numbers or booleans")),
            }
        };
        let list = |v: &toml::Value| -> Result<Vec<String>> {
            match v {
                toml::Value::Array(items) => items.iter().map(scalar).collect(),
                _ => Err(invalid("expected an array")),
            }
        };
        match value {
            toml::Value::Array(_) => Ok(LoopSpec::List(list(value)?)),
            toml::Value::Table(table) => {
                if table.len() != 1 {
                    return Err(invalid(
                        "expected exactly one of `glob`, `matrix` or `from`",
                    ));
                }
                let (key, v) = table.iter().next().expect("one entry");
                match key.as_str() {
                    "glob" => match v {
                        toml::Value::String(s) => Ok(LoopSpec::Glob(vec![s.clone()])),
                        other => Ok(LoopSpec::Glob(list(other)?)),
                    },
                    "from" => match v {
                        toml::Value::String(task) if !task.trim().is_empty() => {
                            Ok(LoopSpec::From(task.trim().to_string()))
                        }
                        _ => Err(invalid("`from` must name a task")),
                    },
                    "matrix" => {
                        let toml::Value::Table(axes) = v else {
                            return Err(invalid("`matrix` must be a table of arrays"));
                        };
                        let mut out = Vec::new();
                        for (axis, values) in axes {
                            out.push((axis.clone(), list(values)?));
                        }
                        out.sort();
                        Ok(LoopSpec::Matrix(out))
                    }
                    other => Err(invalid(&format!(
                        "unknown key '{}' (expected `glob`, `matrix` or `from`)",
                        other
                    ))),
                }
            }
            _ => Err(invalid(
                "expected an array, `{ glob = ... }`, `{ matrix = ... }` or `{ from = ... }`",
            )),
        }
    }
}

/// Document-wide defaults parsed from a `meta` code block that appears
/// before the first heading in the file. Applies to every task; a task's
/// own `meta` block overrides same-named keys, and CLI flags override both.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct GlobalMeta {
    /// Default environment variables applied to every task
    #[serde(default)]
    env: Vec<String>,
    /// Default working directory applied to every task without its own `dir`
    #[serde(default)]
    dir: Option<String>,
    /// Other task files whose tasks are made available as `<name>:<task>`
    #[serde(default)]
    imports: Vec<String>,
    /// `.env` files loaded for every task
    #[serde(default)]
    dotenv: Vec<String>,
    /// Variables available to every task
    #[serde(default)]
    vars: Option<toml::Table>,
}

/// Task metadata parsed from a `meta` code block within a section
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TaskMeta {
    #[serde(default)]
    depends: Vec<String>,
    #[serde(default)]
    params: Vec<toml::Value>,
    #[serde(default)]
    alias: Vec<String>,
    #[serde(default)]
    private: bool,
    /// Default environment variables, e.g. `env = ["REGION=eu", "DEBUG=1"]`
    #[serde(default)]
    env: Vec<String>,
    /// Working directory to run this task's commands in, e.g. `dir = "services/api"`
    #[serde(default)]
    dir: Option<String>,
    /// Run this task's dependencies concurrently
    #[serde(default)]
    parallel: bool,
    /// Condition that must hold for the task to run
    #[serde(default, rename = "if")]
    condition: Option<String>,
    /// How many times to retry after a failure
    #[serde(default)]
    retries: u32,
    /// Delay between retries, e.g. `"2s"`
    #[serde(default)]
    retry_delay: Option<String>,
    /// Kill the task if it runs longer than this, e.g. `"30s"`
    #[serde(default)]
    timeout: Option<String>,
    /// Expose this task's stdout to dependents as `MX_OUTPUT_<TASK>`
    #[serde(default)]
    capture: bool,
    /// Globs of input files; unchanged inputs let the task be skipped
    #[serde(default)]
    sources: Vec<String>,
    /// Globs of files the task produces; must exist for it to be skipped
    #[serde(default)]
    outputs: Vec<String>,
    /// Run the task once per item: an array, `{ glob = ... }` or `{ matrix = ... }`
    #[serde(default, rename = "for")]
    for_each: Option<toml::Value>,
    /// Extra environment variable name for the loop item, e.g. `as = "file"`
    #[serde(default, rename = "as")]
    loop_var: Option<String>,
    /// Tasks run after this one finishes, even if it failed
    #[serde(default)]
    defer: Vec<String>,
    /// Treat a failed run (after retries) as success
    #[serde(default)]
    ignore_error: bool,
    /// Ask for confirmation before running, e.g. `prompt = "Deploy to production?"`
    #[serde(default)]
    prompt: Option<String>,
    /// `.env` files loaded for this task, e.g. `dotenv = [".env", ".env.local"]`
    #[serde(default)]
    dotenv: Vec<String>,
    /// Shell commands; the task is up to date when all of them exit 0
    #[serde(default)]
    status: Vec<String>,
    /// Variables: fixed values or `{ sh = "command" }`
    #[serde(default)]
    vars: Option<toml::Table>,
    /// Run only once per run, however it is called
    #[serde(default)]
    once: bool,
    /// Variables that must be set (and not empty), e.g. `["API_TOKEN"]`
    #[serde(default)]
    requires: Vec<String>,
}

/// A named task parameter, e.g. `params = ["env=staging", "verbose"]`.
/// A bare name is required; `name=value` supplies a default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ParamDef {
    pub name: String,
    pub default: Option<String>,
    /// The values the parameter accepts; any value when empty.
    #[serde(default)]
    pub choices: Vec<String>,
    /// What the parameter is for, shown by `--summary` and the TUI.
    #[serde(default)]
    pub description: Option<String>,
}

impl ParamDef {
    /// The short form: `"name"` (required) or `"name=default"`.
    fn parse(raw: &str) -> Self {
        match raw.split_once('=') {
            Some((name, default)) => ParamDef {
                name: name.trim().to_string(),
                default: Some(default.trim().to_string()),
                ..Default::default()
            },
            None => ParamDef {
                name: raw.trim().to_string(),
                ..Default::default()
            },
        }
    }

    /// The table form: `{ name = "env", default = "dev", choices = [...], desc = "..." }`.
    fn from_toml(task: &str, value: &toml::Value) -> Result<Self> {
        let invalid = |msg: &str| Error::Invalid(format!("task '{}': `params`: {}", task, msg));
        match value {
            toml::Value::String(raw) => Ok(Self::parse(raw)),
            toml::Value::Table(table) => {
                const KEYS: [&str; 4] = ["name", "default", "choices", "desc"];
                let text = |key: &str| -> Result<Option<String>> {
                    match table.get(key) {
                        None => Ok(None),
                        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
                        Some(_) => Err(invalid(&format!("`{}` must be a string", key))),
                    }
                };
                for key in table.keys() {
                    if !KEYS.contains(&key.as_str()) {
                        let hint = crate::util::closest(key, KEYS)
                            .map(|c| format!(" (did you mean `{}`?)", c))
                            .unwrap_or_else(|| {
                                " (expected `name`, `default`, `choices` or `desc`)".into()
                            });
                        return Err(invalid(&format!("unknown key `{}`{}", key, hint)));
                    }
                }
                let name = text("name")?
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .ok_or_else(|| invalid("a table needs a `name`"))?;
                let choices = match table.get("choices") {
                    None => Vec::new(),
                    Some(toml::Value::Array(items)) => items
                        .iter()
                        .map(|v| match v {
                            toml::Value::String(s) => Ok(s.clone()),
                            other => Ok(other.to_string()),
                        })
                        .collect::<Result<Vec<_>>>()?,
                    Some(_) => return Err(invalid("`choices` must be an array")),
                };
                if table.contains_key("choices") && choices.is_empty() {
                    return Err(invalid(&format!("`choices` of '{}' is empty", name)));
                }
                let default = text("default")?;
                if let Some(d) = &default
                    && !choices.is_empty()
                    && !choices.contains(d)
                {
                    return Err(invalid(&format!(
                        "the default '{}' of '{}' is not one of its choices ({})",
                        d,
                        name,
                        choices.join(", ")
                    )));
                }
                Ok(ParamDef {
                    name,
                    default,
                    choices,
                    description: text("desc")?,
                })
            }
            _ => Err(invalid("expected a string or a table")),
        }
    }

    /// All of `params` on one line. The entries are separated by commas, or
    /// by semicolons when some carry choices or a description (which can
    /// contain commas).
    pub fn describe_all(params: &[ParamDef]) -> String {
        let rich = params
            .iter()
            .any(|p| !p.choices.is_empty() || p.description.is_some());
        let separator = if rich { "; " } else { ", " };
        params
            .iter()
            .map(ParamDef::describe)
            .collect::<Vec<_>>()
            .join(separator)
    }

    /// One line for `--summary` and the TUI: `env=dev [dev|prod] - target`.
    pub fn describe(&self) -> String {
        let mut text = match &self.default {
            Some(default) => format!("{}={}", self.name, default),
            None => format!("{} (required)", self.name),
        };
        if !self.choices.is_empty() {
            text.push_str(&format!(" [{}]", self.choices.join("|")));
        }
        if let Some(description) = self.description.as_deref().filter(|d| !d.is_empty()) {
            text.push_str(&format!(" - {}", description));
        }
        text
    }
}

/// Represents a section with its code blocks
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Section {
    /// Section title
    pub title: String,
    /// Code blocks in this section (excludes `meta` metadata blocks)
    pub codes: Vec<CodeBlock>,
    /// Optional description extracted from the section content
    pub description: Option<String>,
    /// Task names this section depends on (declared in a `meta` code block)
    #[serde(default)]
    pub depends: Vec<String>,
    /// Named parameters this task accepts (declared in a `meta` code block)
    #[serde(default)]
    pub params: Vec<ParamDef>,
    /// Alternate names this task can be run by (declared in a `meta` code block)
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Hidden from `list`/`tui` output; set via `meta` or a `_`-prefixed title
    #[serde(default)]
    pub private: bool,
    /// Default environment variables declared in a `meta` code block;
    /// overridden by CLI `--env` flags of the same name
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Working directory declared in a `meta` code block; overridden by
    /// the CLI `--dir` flag
    #[serde(default)]
    pub dir: Option<PathBuf>,
    /// Run this task's dependencies concurrently
    #[serde(default)]
    pub parallel: bool,
    /// `if` condition; the task is skipped when it evaluates to false
    #[serde(default)]
    pub condition: Option<String>,
    /// Number of retries after a failed attempt
    #[serde(default)]
    pub retries: u32,
    /// Delay between retries (defaults to one second)
    #[serde(default)]
    pub retry_delay: Option<Duration>,
    /// Maximum run time per attempt
    #[serde(default)]
    pub timeout: Option<Duration>,
    /// Capture stdout and hand it to dependents as `MX_OUTPUT_<TASK>`
    #[serde(default)]
    pub capture: bool,
    /// Input globs for up-to-date checks and `--watch`
    #[serde(default)]
    pub sources: Vec<String>,
    /// Output globs that must exist for the task to count as up to date
    #[serde(default)]
    pub outputs: Vec<String>,
    /// `for` loop: the task runs once per item
    #[serde(default)]
    pub for_each: Option<LoopSpec>,
    /// Extra name for the loop item (`as = "file"` sets `MX_FILE`)
    #[serde(default)]
    pub loop_var: Option<String>,
    /// Tasks (optionally with arguments) run after this one finishes,
    /// whatever its outcome
    #[serde(default)]
    pub defer: Vec<String>,
    /// A failed run (after retries) is reported but not treated as a failure
    #[serde(default)]
    pub ignore_error: bool,
    /// Confirmation message; the task runs only on yes or `--yes`
    #[serde(default)]
    pub prompt: Option<String>,
    /// `.env` files, relative to the task's working directory
    #[serde(default)]
    pub dotenv: Vec<String>,
    /// Shell commands; the task is skipped when all exit 0
    #[serde(default)]
    pub status: Vec<String>,
    /// Runs once per run even if called with different arguments (first wins)
    #[serde(default)]
    pub once: bool,
    /// Variables that must be set and not empty
    #[serde(default)]
    pub requires: Vec<String>,
    /// Named values for the task, fixed or computed by a shell command
    #[serde(default)]
    pub vars: Vec<VarDef>,
}

/// Document-wide settings found in the preamble `meta` block.
#[derive(Debug, Default)]
struct Globals {
    env: Vec<(String, String)>,
    dir: Option<PathBuf>,
    imports: Vec<String>,
    dotenv: Vec<String>,
    vars: Vec<VarDef>,
}

/// Options for [`Runner::run_task_with_options`].
#[derive(Default)]
pub struct RunOptions {
    /// Arguments for the requested task (`MX_ARGS`, params)
    pub args: Vec<String>,
    /// Only run code blocks of this language
    pub lang_filter: Option<String>,
    /// Run dependencies concurrently regardless of each task's `parallel`
    pub parallel: bool,
    /// Maximum concurrent tasks; `Some(1)` forces sequential execution
    pub jobs: Option<usize>,
    /// Keep running independent tasks after one fails
    pub keep_going: bool,
    /// How task output is presented on the terminal
    pub output: OutputMode,
    /// Receives progress and task output instead of the terminal. Task
    /// output is piped through it and stdin is closed.
    pub observer: Option<Arc<dyn RunObserver>>,
    /// Set to abort the run: running processes are killed and no further
    /// tasks start.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Print only task output and failures
    pub quiet: bool,
    /// Print how long each task took when the run ends
    pub timings: bool,
    /// Run only the requested tasks, not their `depends`
    pub no_deps: bool,
}

/// Whether one task would run, as reported by [`Runner::task_states`].
#[derive(Debug, Clone, PartialEq)]
pub struct TaskState {
    pub task: String,
    /// True when the task would be skipped (up to date, or its `if` is false).
    pub up_to_date: bool,
    pub detail: String,
}

/// Collects the verdict on each task of a `--status` check.
#[derive(Default)]
struct StateRecorder {
    states: std::sync::Mutex<Vec<TaskState>>,
}

impl RunObserver for StateRecorder {
    fn output(&self, _task: &str, _stream: crate::events::Stream, _line: &str) {}

    fn status(&self, task: &str, status: &crate::events::TaskStatus) {
        use crate::events::TaskStatus;
        let (up_to_date, detail) = match status {
            TaskStatus::Skipped { reason } => (true, reason.clone()),
            TaskStatus::Failed { error } => (false, error.clone()),
            TaskStatus::Cancelled { .. } => (
                false,
                "not checked: a dependency is out of date".to_string(),
            ),
            _ => return,
        };
        self.states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(TaskState {
                task: task.to_string(),
                up_to_date,
                detail,
            });
    }
}

/// A set of files whose changes should re-run a task in watch mode.
#[derive(Debug, Clone)]
pub struct WatchTarget {
    pub patterns: Vec<String>,
    pub dir: Option<PathBuf>,
    /// `patterns` are exact file names (`dotenv`), not globs (`sources`).
    pub literal: bool,
}

impl WatchTarget {
    /// Path, mtime and size of every watched file that exists.
    pub fn snapshot(&self) -> std::collections::HashMap<PathBuf, crate::cache::FileStamp> {
        if self.literal {
            crate::cache::snapshot_files(&self.patterns, self.dir.as_deref())
        } else {
            crate::cache::snapshot(&self.patterns, self.dir.as_deref())
        }
    }
}

/// Task runner that executes code blocks in Markdown sections
pub struct Runner {
    engine: Engine,
    exec: Executor,
    /// `imports` of the file most recently passed to `extract_sections`.
    imports: Vec<String>,
    /// When set, loading records problems here and carries on (`check`).
    collected: Option<Collected>,
}

/// Problems found while loading in lenient mode.
#[derive(Default)]
struct Collected {
    errors: Vec<Error>,
    /// Titles of tasks that could not be parsed.
    failed: Vec<String>,
}

impl Runner {
    /// Create a new Runner with the given configuration
    pub fn new(config: Config) -> Self {
        let mut engine: Engine = Engine::default();
        engine.load_builtin_module();

        Self {
            engine,
            exec: Executor::new(config),
            imports: Vec::new(),
            collected: None,
        }
    }

    /// Create a new Runner with default configuration
    pub fn with_default_config() -> Self {
        Self::new(Config::default())
    }

    /// Enable or disable dry-run mode
    pub fn set_dry_run(&mut self, dry_run: bool) {
        self.exec.dry_run = dry_run;
    }

    /// Enable or disable `sources`/`outputs` up-to-date checks (default: on)
    pub fn set_use_cache(&mut self, use_cache: bool) {
        self.exec.use_cache = use_cache;
    }

    /// Show each code block before it runs (`-v`).
    pub fn set_verbose(&mut self, verbose: bool) {
        self.exec.verbose = verbose;
    }

    /// Answer yes to every `prompt` without asking (`--yes`).
    pub fn set_assume_yes(&mut self, assume_yes: bool) {
        self.exec.assume_yes = assume_yes;
    }

    /// Set environment variables injected into every task process for this run,
    /// e.g. from repeated `--env KEY=VALUE` CLI flags. Unlike declared `params`,
    /// these need no `meta` declaration in the task file.
    pub fn set_env_overrides(&mut self, env_overrides: Vec<(String, String)>) {
        self.exec.env_overrides = env_overrides;
    }

    /// Parse `KEY=VALUE` strings (as passed via repeated `--env` flags) into pairs.
    pub fn parse_env_overrides(raw: &[String]) -> Result<Vec<(String, String)>> {
        raw.iter()
            .map(|entry| match entry.split_once('=') {
                Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
                _ => Err(Error::InvalidEnv(entry.clone())),
            })
            .collect()
    }

    /// Set the working directory task processes are spawned in for this run,
    /// e.g. from a `--dir PATH` CLI flag. Applies to every code block executed.
    pub fn set_working_dir(&mut self, working_dir: Option<std::path::PathBuf>) {
        self.exec.working_dir = working_dir;
    }

    /// Load and parse a Markdown file
    pub fn load_markdown<P: AsRef<Path>>(&self, path: P) -> Result<String> {
        fs::read_to_string(path).map_err(Error::Io)
    }

    /// Extract sections from Markdown content. Does not follow `imports`;
    /// use [`Runner::load_sections`] to load a file with its imports.
    pub fn extract_sections(&mut self, markdown: &str) -> Result<Vec<Section>> {
        let input = parse_markdown_input(markdown)
            .map_err(|e| Error::Markdown(format!("Failed to parse markdown: {}", e)))?;

        let globals = match self.parse_global_meta(input.clone()) {
            Ok(globals) => globals,
            Err(e) => match self.collected.as_mut() {
                Some(collected) => {
                    collected.errors.push(e);
                    Globals::default()
                }
                None => return Err(e),
            },
        };
        self.exec.global_env = globals.env;
        self.exec.global_dir = globals.dir;
        self.exec.global_dotenv = globals.dotenv;
        self.exec.global_vars = globals.vars;
        self.imports = globals.imports;

        let query = format!("{}\n | nodes | sections_with_code()", SECTIONS_QUERY);
        let result = self
            .engine
            .eval(&query, input.into_iter())
            .map_err(|e| Error::Query(format!("Failed to execute query: {}", e)))?;

        let sections = self.parse_sections(result)?;

        Ok(sections)
    }

    /// Like [`Runner::load_sections`], but carries on after a broken task or
    /// import. Returns the tasks that loaded, the problems found and the
    /// titles of the tasks that could not be parsed.
    pub fn load_sections_lenient<P: AsRef<Path>>(
        &mut self,
        path: P,
    ) -> (Vec<Section>, Vec<Error>, Vec<String>) {
        self.collected = Some(Collected::default());
        let mut stack = Vec::new();
        let sections = match self.load_file(path.as_ref(), &mut stack) {
            Ok((sections, _)) => sections,
            Err(e) => {
                if let Some(collected) = self.collected.as_mut() {
                    collected.errors.push(e);
                }
                Vec::new()
            }
        };
        let collected = self.collected.take().unwrap_or_default();
        (sections, collected.errors, collected.failed)
    }

    /// Load the tasks of a Markdown file together with those of the files
    /// it `imports`. Imported tasks are named `<file stem>:<task>`.
    pub fn load_sections<P: AsRef<Path>>(&mut self, path: P) -> Result<Vec<Section>> {
        let mut stack = Vec::new();
        let (sections, _) = self.load_file(path.as_ref(), &mut stack)?;
        // Loading imports overwrote the document defaults; the main file's
        // own defaults apply at run time, so restore them.
        let markdown = self.load_markdown(path.as_ref())?;
        let input = parse_markdown_input(&markdown)
            .map_err(|e| Error::Markdown(format!("Failed to parse markdown: {}", e)))?;
        let globals = self.parse_global_meta(input)?;
        self.exec.global_env = globals.env;
        self.exec.global_dir = globals.dir;
        self.exec.global_dotenv = globals.dotenv;
        self.exec.global_vars = globals.vars;
        // Spelling the path differently (`tasks.md`, `./tasks.md`) must not
        // give the same file a second set of fingerprints.
        let scope = path
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| path.as_ref().to_path_buf());
        self.exec.cache_scope = scope.to_string_lossy().into_owned();
        Ok(sections)
    }

    /// Returns the file's sections (imports merged in, already namespaced)
    /// and its document-wide defaults, which the caller applies.
    fn load_file(
        &mut self,
        path: &Path,
        stack: &mut Vec<PathBuf>,
    ) -> Result<(Vec<Section>, Globals)> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if stack.contains(&canonical) {
            return Err(Error::Invalid(format!(
                "circular import of {}",
                path.display()
            )));
        }
        stack.push(canonical);

        let markdown = self.load_markdown(path)?;
        let mut sections = self.extract_sections(&markdown)?;
        let globals = Globals {
            env: self.exec.global_env.clone(),
            dir: self.exec.global_dir.clone(),
            imports: std::mem::take(&mut self.imports),
            dotenv: self.exec.global_dotenv.clone(),
            vars: self.exec.global_vars.clone(),
        };

        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let mut namespaces = HashSet::new();
        for import in &globals.imports {
            let import_path = base.join(import);
            let stem = import_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .ok_or_else(|| Error::Invalid(format!("invalid import path '{}'", import)))?;
            if !namespaces.insert(stem.clone()) {
                let e = Error::Invalid(format!("two imports share the namespace '{}'", stem));
                match self.collected.as_mut() {
                    Some(collected) => {
                        collected.errors.push(e);
                        continue;
                    }
                    None => return Err(e),
                }
            }

            let (mut imported, child_globals) = match self.load_file(&import_path, stack) {
                Ok(loaded) => loaded,
                Err(e) => match self.collected.as_mut() {
                    Some(collected) => {
                        collected
                            .errors
                            .push(Error::Invalid(format!("import '{}': {}", import, e)));
                        continue;
                    }
                    None => return Err(e),
                },
            };
            // Importing a file's own heading (its document title) as a task
            // is noise; keep only sections that do something.
            imported.retain(|s| !s.codes.is_empty() || !s.depends.is_empty());
            apply_globals(&mut imported, &child_globals);
            namespace_sections(&mut imported, &stem);
            sections.extend(imported);
        }

        stack.pop();
        Ok((sections, globals))
    }

    /// Parse the document-wide `meta` block declared before the first
    /// heading, if any, into defaults for every task.
    fn parse_global_meta(&mut self, input: Vec<RuntimeValue>) -> Result<Globals> {
        let query = format!(
            "{}\n | nodes | {{\"codes\": global_code_blocks()}}",
            SECTIONS_QUERY
        );
        let result = self
            .engine
            .eval(&query, input.into_iter())
            .map_err(|e| Error::Query(format!("Failed to execute query: {}", e)))?;

        let codes = result
            .into_iter()
            .find_map(|value| match value {
                RuntimeValue::Dict(dict) => dict.get(&Ident::from("codes")).and_then(|v| match v {
                    RuntimeValue::Array(arr) => self.parse_code_blocks(arr).ok(),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap_or_default();

        let meta = match codes.iter().find(|c| c.lang == "meta") {
            Some(block) => toml::from_str::<GlobalMeta>(&block.code)
                .map_err(|e| meta_error("document `meta` block", &block.code, &e, true))?,
            None => GlobalMeta::default(),
        };

        Ok(Globals {
            env: Self::parse_env_overrides(&meta.env)?,
            dir: meta.dir.map(PathBuf::from),
            imports: meta.imports,
            dotenv: meta.dotenv,
            vars: match &meta.vars {
                Some(table) => vars::parse_table("document `meta` block", table)?,
                None => Vec::new(),
            },
        })
    }

    fn parse_sections(&mut self, result: mq_lang::RuntimeValues) -> Result<Vec<Section>> {
        let mut sections = Vec::new();

        for value in result.into_iter() {
            if let RuntimeValue::Dict(dict) = value {
                match self.parse_section(&dict) {
                    Ok(section) => sections.push(section),
                    Err(e) => match self.collected.as_mut() {
                        Some(collected) => {
                            if let Some(RuntimeValue::String(title)) =
                                dict.get(&Ident::from("title"))
                            {
                                collected.failed.push(title.to_string());
                            }
                            collected.errors.push(e);
                        }
                        None => return Err(e),
                    },
                }
            }
        }

        Ok(sections)
    }

    fn parse_section(&self, dict: &DictMap) -> Result<Section> {
        let title = dict
            .get(&Ident::from("title"))
            .and_then(|v| match v {
                RuntimeValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_default();

        let all_codes = dict
            .get(&Ident::from("codes"))
            .and_then(|v| match v {
                RuntimeValue::Array(arr) => Some(self.parse_code_blocks(arr)),
                _ => None,
            })
            .unwrap_or_else(|| Ok(Vec::new()))?;

        let meta = match all_codes.iter().find(|c| c.lang == "meta") {
            Some(block) => toml::from_str::<TaskMeta>(&block.code).map_err(|e| {
                meta_error(
                    &format!("task '{}': `meta` block", title),
                    &block.code,
                    &e,
                    false,
                )
            })?,
            None => TaskMeta::default(),
        };

        let duration = |value: &Option<String>| -> Result<Option<Duration>> {
            value
                .as_deref()
                .map(parse_duration)
                .transpose()
                .map_err(|e| Error::Invalid(format!("task '{}': {}", title, e)))
        };
        let retry_delay = duration(&meta.retry_delay)?;
        let timeout = duration(&meta.timeout)?;

        let params = meta
            .params
            .iter()
            .map(|p| ParamDef::from_toml(&title, p))
            .collect::<Result<Vec<_>>>()?;
        let private = meta.private || title.starts_with('_');
        let env = Self::parse_env_overrides(&meta.env)?;
        let dir = meta.dir.map(PathBuf::from);

        let vars = match &meta.vars {
            Some(table) => vars::parse_table(&format!("task '{}': `meta` block", title), table)?,
            None => Vec::new(),
        };

        let for_each = meta
            .for_each
            .as_ref()
            .map(|v| LoopSpec::from_toml(&title, v))
            .transpose()?;
        if meta.loop_var.is_some() && for_each.is_none() {
            return Err(Error::Invalid(format!(
                "task '{}': `as` requires `for`",
                title
            )));
        }

        let codes: Vec<CodeBlock> = all_codes
            .into_iter()
            .filter(|c| c.lang != "meta")
            .filter(|c| {
                self.exec.config.has_runtime(&c.lang) || !DATA_LANGUAGES.contains(&c.lang.as_str())
            })
            .collect();

        let description = dict.get(&Ident::from("description")).and_then(|v| match v {
            RuntimeValue::String(s) => Some(s.to_string()),
            _ => None,
        });

        Ok(Section {
            title,
            codes,
            description,
            depends: meta.depends,
            params,
            aliases: meta.alias,
            private,
            env,
            dir,
            parallel: meta.parallel,
            condition: meta.condition,
            retries: meta.retries,
            retry_delay,
            timeout,
            capture: meta.capture,
            sources: meta.sources,
            outputs: meta.outputs,
            for_each,
            loop_var: meta.loop_var,
            defer: meta.defer,
            ignore_error: meta.ignore_error,
            prompt: meta.prompt,
            dotenv: meta.dotenv,
            status: meta.status,
            once: meta.once,
            requires: meta.requires,
            vars,
        })
    }

    fn parse_code_blocks(&self, arr: &[RuntimeValue]) -> Result<Vec<CodeBlock>> {
        let mut blocks = Vec::new();

        for item in arr {
            if let RuntimeValue::Dict(dict) = item {
                let lang = dict
                    .get(&Ident::from("lang"))
                    .and_then(|v| match v {
                        RuntimeValue::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let code = dict
                    .get(&Ident::from("code"))
                    .and_then(|v| match v {
                        RuntimeValue::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let no_run = dict.get(&Ident::from("info")).is_some_and(|v| match v {
                    RuntimeValue::String(s) => s.split_whitespace().any(|w| w == NO_RUN),
                    _ => false,
                });
                if no_run {
                    continue;
                }

                blocks.push(CodeBlock { lang, code });
            }
        }

        Ok(blocks)
    }

    pub fn find_section<'a>(&self, sections: &'a [Section], title: &str) -> Option<&'a Section> {
        schedule::find_section(sections, title)
    }

    pub fn execute_section(&self, section: &Section) -> Result<()> {
        self.execute_section_with_args(section, &[])
    }

    pub fn execute_section_with_args(&self, section: &Section, args: &[String]) -> Result<()> {
        self.execute_section_with_lang_filter(section, args, None)
    }

    pub fn execute_section_with_lang_filter(
        &self,
        section: &Section,
        args: &[String],
        lang_filter: Option<&str>,
    ) -> Result<()> {
        self.exec.execute_section(section, args, lang_filter)
    }

    pub fn execute_code(&self, lang: &str, code: &str) -> Result<()> {
        self.execute_code_with_args(lang, code, &[])
    }

    pub fn execute_code_with_args(&self, lang: &str, code: &str, args: &[String]) -> Result<()> {
        self.exec
            .execute_code(lang, code, args, &[], self.exec.working_dir.as_deref())
    }

    /// Plan `target`, resolving `for` globs where the tasks run.
    fn plan_for<'a>(
        &'a self,
        sections: &'a [Section],
        target: &str,
        force_parallel: bool,
    ) -> Result<schedule::Plan<'a>> {
        let ctx = PlanContext {
            working_dir: self.exec.working_dir.as_deref(),
            global_dir: self.exec.global_dir.as_deref(),
        };
        build_plan_in(sections, target, force_parallel, &ctx)
    }

    /// Run a specific task by section title
    pub fn run_task<P: AsRef<Path>>(&mut self, markdown_path: P, task_name: &str) -> Result<()> {
        self.run_task_with_args(markdown_path, task_name, &[])
    }

    /// Run a specific task with arguments
    pub fn run_task_with_args<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        task_name: &str,
        args: &[String],
    ) -> Result<()> {
        self.run_task_with_lang_filter(markdown_path, task_name, args, None)
    }

    /// Run a specific task with arguments and language filter
    pub fn run_task_with_lang_filter<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        task_name: &str,
        args: &[String],
        lang_filter: Option<&str>,
    ) -> Result<()> {
        self.run_task_with_options(
            markdown_path,
            task_name,
            RunOptions {
                args: args.to_vec(),
                lang_filter: lang_filter.map(str::to_string),
                ..Default::default()
            },
        )
    }

    /// Run a task and its dependencies. Dependencies run in declaration
    /// order unless the task sets `parallel` (or `opts` asks for it), in
    /// which case independent tasks run concurrently.
    pub fn run_task_with_options<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        task_name: &str,
        opts: RunOptions,
    ) -> Result<()> {
        self.run_tasks_with_options(markdown_path, &[task_name.to_string()], opts)
    }

    /// Run several tasks one after another as a single run, so a dependency
    /// they share runs once. With one task this is [`Self::run_task_with_options`].
    /// `opts.args` belong to a single task and are rejected for several.
    pub fn run_tasks_with_options<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        tasks: &[String],
        opts: RunOptions,
    ) -> Result<()> {
        if tasks.len() > 1 && !opts.args.is_empty() {
            return Err(Error::Invalid(
                "arguments after `--` can only be given to a single task".to_string(),
            ));
        }
        let (sections, task_name) = self.sections_for(markdown_path, tasks, opts.no_deps)?;
        let task_name = task_name.as_str();
        self.exec.clear_var_cache();

        let explicit_parallel = opts.parallel || opts.jobs.is_some_and(|j| j > 1);
        let mut plan = self.plan_for(&sections, task_name, explicit_parallel)?;
        let aggregate = tasks.len() > 1;
        if aggregate {
            // The tasks asked for are what the user sees as the run.
            let root = plan.root();
            for id in plan.nodes[root].deps.clone() {
                plan.nodes[id].requested = true;
            }
        }

        let wants_parallel = explicit_parallel || plan.nodes.iter().any(|n| n.section.parallel);
        // Dry-run output goes straight to stdout; keep it ordered.
        let jobs = if self.exec.dry_run || plan.nodes.len() == 1 {
            1
        } else {
            match opts.jobs {
                Some(j) => j.max(1),
                None if wants_parallel => std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4),
                None => 1,
            }
        };

        let names = plan.names();
        let mode = opts.output;
        // Prefixed and grouped output need piped task output.
        let custom_observer =
            opts.observer.is_some() || matches!(mode, OutputMode::Prefixed | OutputMode::Group);
        let observer: Arc<dyn RunObserver> = match opts.observer {
            Some(o) => o,
            None if mode == OutputMode::Group => Arc::new(
                ConsoleObserver::grouped()
                    .quiet(opts.quiet)
                    .timings(opts.timings),
            ),
            None if mode == OutputMode::Prefixed || (mode == OutputMode::Auto && jobs > 1) => {
                Arc::new(
                    ConsoleObserver::prefixed_for(&names)
                        .quiet(opts.quiet)
                        .timings(opts.timings),
                )
            }
            None => {
                let root = plan.nodes[plan.root()].section.title.as_str();
                if !opts.quiet {
                    println!("{} {}", "▶".cyan().bold(), root.bold());
                }
                let console = ConsoleObserver::new()
                    .quiet(opts.quiet)
                    .timings(opts.timings);
                Arc::new(if aggregate {
                    console.announcing_requested(root)
                } else {
                    console
                })
            }
        };
        observer.plan(&names);
        observer.graph(&plan.edges());
        let no_cancel = AtomicBool::new(false);
        let cancel = opts.cancel.as_deref().unwrap_or(&no_cancel);

        let tracker = FailureTracker::new(observer.as_ref());
        let started = Instant::now();
        let result = schedule::run_plan(
            &self.exec,
            &plan,
            &ScheduleOptions {
                args: &opts.args,
                lang_filter: opts.lang_filter.as_deref(),
                jobs,
                keep_going: opts.keep_going,
                piped: custom_observer,
                cancel,
                observer: &tracker,
            },
        );
        observer.finished(started.elapsed());
        result.map_err(|err| match tracker.first_failure() {
            Some(task) => err.in_task(&task),
            None => err,
        })
    }

    /// The file's sections and the task to plan for running `tasks`. For
    /// several tasks that is a section added here which depends on them in
    /// order, so they share one plan.
    fn sections_for<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        tasks: &[String],
        no_deps: bool,
    ) -> Result<(Vec<Section>, String)> {
        let mut sections = self.load_sections(markdown_path)?;
        if no_deps {
            for task in tasks {
                let found = self.find_section(&sections, task).map(|s| s.title.clone());
                for section in sections
                    .iter_mut()
                    .filter(|s| Some(&s.title) == found.as_ref())
                {
                    section.depends.clear();
                }
            }
        }
        let [only] = tasks else {
            for task in tasks {
                if self.find_section(&sections, task).is_none() {
                    return Err(Error::SectionNotFound(task.clone()));
                }
            }
            let title = tasks.join(", ");
            sections.push(Section {
                title: title.clone(),
                depends: tasks.to_vec(),
                ..Default::default()
            });
            return Ok((sections, title));
        };
        Ok((sections, only.clone()))
    }

    /// Say, without running anything, whether each task of the run for
    /// `tasks` would run (`--status`). A task is up to date when its
    /// `status` commands or its `sources` and `outputs` say so, or when its
    /// `if` is false; a task with none of these always runs. The tasks that
    /// depend on one that is out of date are not checked.
    pub fn task_states<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        tasks: &[String],
        opts: &RunOptions,
    ) -> Result<Vec<TaskState>> {
        let (sections, task_name) = self.sections_for(markdown_path, tasks, opts.no_deps)?;
        self.exec.clear_var_cache();
        let plan = self.plan_for(&sections, &task_name, false)?;

        let recorder = StateRecorder::default();
        let no_cancel = AtomicBool::new(false);
        let mut exec = self.exec.clone();
        exec.check_only = true;
        let _ = schedule::run_plan(
            &exec,
            &plan,
            &ScheduleOptions {
                args: &opts.args,
                lang_filter: opts.lang_filter.as_deref(),
                jobs: 1,
                keep_going: true,
                piped: true,
                cancel: &no_cancel,
                observer: &recorder,
            },
        );

        let mut states = recorder.states.into_inner().unwrap_or_default();
        if tasks.len() > 1 {
            // The task added to run them together is not one of theirs.
            states.retain(|s| s.task != task_name);
        }
        Ok(states)
    }

    /// Files whose changes should re-run the tasks in watch mode: every
    /// `sources` glob and `dotenv` file of the task and its dependencies,
    /// plus the task file.
    pub fn watch_targets<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        task_name: &str,
    ) -> Result<Vec<WatchTarget>> {
        self.watch_targets_for(markdown_path, &[task_name.to_string()], false)
    }

    /// [`Self::watch_targets`] for several tasks run together, optionally
    /// without their dependencies (`--no-deps`).
    pub fn watch_targets_for<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        tasks: &[String],
        no_deps: bool,
    ) -> Result<Vec<WatchTarget>> {
        let (sections, task_name) = self.sections_for(markdown_path.as_ref(), tasks, no_deps)?;
        let plan = self.plan_for(&sections, &task_name, false)?;

        let mut targets = vec![WatchTarget {
            patterns: vec![markdown_path.as_ref().to_string_lossy().into_owned()],
            dir: None,
            literal: true,
        }];
        for node in &plan.nodes {
            let dir = self.exec.effective_dir(node.section).map(Path::to_path_buf);
            if !node.section.sources.is_empty() {
                targets.push(WatchTarget {
                    patterns: node.section.sources.clone(),
                    dir: dir.clone(),
                    literal: false,
                });
            }
            let dotenv: Vec<String> = self
                .exec
                .global_dotenv
                .iter()
                .chain(&node.section.dotenv)
                .cloned()
                .collect();
            if !dotenv.is_empty() {
                targets.push(WatchTarget {
                    patterns: dotenv,
                    dir,
                    literal: true,
                });
            }
        }
        Ok(targets)
    }

    /// Describe a single task (`--summary`).
    pub fn task_summary<P: AsRef<Path>>(&mut self, markdown_path: P, task: &str) -> Result<String> {
        let sections = self.load_sections(markdown_path)?;
        let section = schedule::find_section(&sections, task)
            .ok_or_else(|| Error::SectionNotFound(task.to_string()))?;
        Ok(crate::summary::render(section))
    }

    /// Render the dependency graph of `task` (or of every public task).
    pub fn dependency_graph<P: AsRef<Path>>(
        &mut self,
        markdown_path: P,
        task: Option<&str>,
        format: GraphFormat,
    ) -> Result<String> {
        let sections = self.load_sections(markdown_path)?;
        if let Some(name) = task {
            // Fail loudly on unknown tasks and cycles.
            self.plan_for(&sections, name, false)?;
        }
        Ok(graph::render(&sections, task, format))
    }

    /// List all available tasks (sections) in a Markdown file
    pub fn list_tasks<P: AsRef<Path>>(&mut self, markdown_path: P) -> Result<Vec<String>> {
        let sections = self.load_sections(markdown_path)?;

        Ok(sections
            .into_iter()
            .map(|s| format!("{}: {}", s.title, s.description.unwrap_or_default()))
            .collect())
    }

    /// List all available task sections in a Markdown file with their details
    pub fn list_task_sections<P: AsRef<Path>>(&mut self, markdown_path: P) -> Result<Vec<Section>> {
        // Headings with neither code nor dependencies (e.g. the document
        // title) do nothing when run, so they are not worth listing.
        let mut sections = self.load_sections(markdown_path)?;
        sections.retain(|s| !s.codes.is_empty() || !s.depends.is_empty());
        Ok(sections)
    }
}

/// Fold an imported file's document-wide defaults into its tasks so they
/// keep behaving as they would when run from that file directly.
fn apply_globals(sections: &mut [Section], globals: &Globals) {
    for section in sections {
        let mut env = globals.env.clone();
        env.append(&mut section.env);
        section.env = env;
        if section.dir.is_none() {
            section.dir = globals.dir.clone();
        }
        let mut dotenv = globals.dotenv.clone();
        dotenv.append(&mut section.dotenv);
        section.dotenv = dotenv;
        let mut vars = globals.vars.clone();
        vars.append(&mut section.vars);
        section.vars = vars;
    }
}

/// Prefix titles, aliases and intra-file dependency references with
/// `<namespace>:`. References to tasks outside the file are left alone.
fn namespace_sections(sections: &mut [Section], namespace: &str) {
    let local: HashSet<String> = sections
        .iter()
        .flat_map(|s| std::iter::once(s.title.clone()).chain(s.aliases.iter().cloned()))
        .collect();
    let qualify = |name: &str| format!("{}:{}", namespace, name);

    let qualify_call = |spec: &str| -> String {
        if local.contains(spec.trim()) {
            return qualify(spec.trim());
        }
        let words = split_words(spec);
        for n in (1..=words.len()).rev() {
            let name = words[..n].join(" ");
            if local.contains(&name) {
                let args = words[n..].iter().map(|w| quote_word(w));
                return std::iter::once(qualify(&name))
                    .chain(args)
                    .collect::<Vec<_>>()
                    .join(" ");
            }
        }
        spec.to_string()
    };

    for section in sections {
        if let Some(LoopSpec::From(source)) = &mut section.for_each
            && local.contains(source.as_str())
        {
            *source = qualify(source);
        }
        section.title = qualify(&section.title);
        section.aliases = section.aliases.iter().map(|a| qualify(a)).collect();
        for dep in section.depends.iter_mut().chain(section.defer.iter_mut()) {
            *dep = qualify_call(dep);
        }
    }
}

/// The key and the valid keys from a serde "unknown field" message such as
/// ``unknown field `retires`, expected one of `retries`, `timeout` ``.
fn unknown_field(message: &str) -> Option<(String, Vec<String>)> {
    let rest = message.strip_prefix("unknown field ")?;
    let mut quoted = rest.split('`').skip(1).step_by(2).map(str::to_string);
    let key = quoted.next()?;
    Some((key, quoted.collect()))
}

/// Turn a `meta` block parse error into one a user can act on: unknown keys
/// get a "did you mean" hint and the line they are on.
fn meta_error(context: &str, code: &str, err: &toml::de::Error, document: bool) -> Error {
    let Some((key, valid)) = unknown_field(err.message()) else {
        return Error::Invalid(format!("{}: {}", context, err));
    };
    let line = err
        .span()
        .map(|s| {
            format!(
                " on line {}",
                code[..s.start.min(code.len())].matches('\n').count() + 1
            )
        })
        .unwrap_or_default();
    let hint = if !document && key == "imports" {
        "`imports` only works in the document-wide `meta` block before the first heading"
            .to_string()
    } else if let Some(close) = crate::util::closest(&key, valid.iter().map(String::as_str)) {
        format!("did you mean `{}`?", close)
    } else {
        "see the Meta keys reference for the valid keys".to_string()
    };
    Error::Invalid(format!(
        "{}: unknown key `{}`{} ({})",
        context, key, line, hint
    ))
}

/// Quote a word for [`split_words`] when it would not survive unquoted.
fn quote_word(word: &str) -> String {
    let plain =
        !word.is_empty() && !word.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'');
    if plain {
        word.to_string()
    } else if word.contains('"') {
        format!("'{}'", word)
    } else {
        format!("\"{}\"", word)
    }
}

#[cfg(test)]
impl Runner {
    fn resolve_execution_order<'a>(
        &self,
        sections: &'a [Section],
        target: &str,
    ) -> Result<Vec<&'a Section>> {
        Ok(schedule::build_plan(sections, target, false)?
            .nodes
            .into_iter()
            .map(|n| n.section)
            .collect())
    }

    fn bind_params(
        params: &[ParamDef],
        args: &[String],
        task_name: &str,
    ) -> Result<Vec<(String, String)>> {
        Executor::bind_params(params, args, task_name)
    }

    fn prepare_env_vars(
        &self,
        args: &[String],
        param_env: &[(String, String)],
    ) -> Vec<(String, String)> {
        self.exec.prepare_env_vars(args, param_env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_sections() {
        let markdown = r#"# Title

## Task 1

```bash
echo "hello"
```

## Task 2

```python
print("world")
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();

        assert_eq!(sections.len(), 3);
        assert_eq!(sections[1].title, "Task 1");
        assert_eq!(sections[1].codes.len(), 1);
        assert_eq!(sections[1].codes[0].lang, "bash");
    }

    #[test]
    fn test_find_section() {
        let sections = vec![
            Section {
                title: "Task 1".to_string(),
                ..Default::default()
            },
            Section {
                title: "Task 2".to_string(),
                ..Default::default()
            },
        ];

        let runner = Runner::with_default_config();
        let found = runner.find_section(&sections, "Task 1");
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "Task 1");

        let not_found = runner.find_section(&sections, "Task 3");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_language_filter() {
        let section = Section {
            title: "Mixed Task".to_string(),
            codes: vec![
                CodeBlock {
                    lang: "bash".to_string(),
                    code: "echo 'bash code'".to_string(),
                },
                CodeBlock {
                    lang: "python".to_string(),
                    code: "print('python code')".to_string(),
                },
                CodeBlock {
                    lang: "bash".to_string(),
                    code: "echo 'more bash'".to_string(),
                },
            ],
            description: None,
            depends: vec![],
            params: vec![],
            aliases: vec![],
            private: false,
            env: vec![],
            dir: None,
            ..Default::default()
        };

        let runner = Runner::with_default_config();

        // Test filtering for bash only - this will fail if bash is not available,
        // but demonstrates the filtering logic
        let result = runner.execute_section_with_lang_filter(&section, &[], Some("bash"));
        // We can't guarantee bash is available in test environment, so we just check
        // that the method runs without panicking
        let _ = result;
    }

    #[test]
    fn test_extract_sections_with_multiple_languages() {
        let markdown = r#"# Title

## Mixed Task

```bash
echo "bash code"
```

```python
print("python code")
```

```bash
echo "more bash"
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();

        assert_eq!(sections.len(), 2);
        assert_eq!(sections[1].title, "Mixed Task");
        assert_eq!(sections[1].codes.len(), 3);
        assert_eq!(sections[1].codes[0].lang, "bash");
        assert_eq!(sections[1].codes[1].lang, "python");
        assert_eq!(sections[1].codes[2].lang, "bash");
    }

    #[test]
    fn test_depends_parsed_from_meta_block() {
        let markdown = r#"# Title

## format

```bash
echo "formatting"
```

## lint

```meta
depends = ["format"]
```

```bash
echo "linting"
```

## test

```meta
depends = ["lint"]
```

```bash
echo "testing"
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();

        // meta blocks should not appear in codes
        let lint = sections.iter().find(|s| s.title == "lint").unwrap();
        assert_eq!(lint.depends, vec!["format"]);
        assert_eq!(lint.codes.len(), 1);
        assert_eq!(lint.codes[0].lang, "bash");

        let test = sections.iter().find(|s| s.title == "test").unwrap();
        assert_eq!(test.depends, vec!["lint"]);
    }

    #[test]
    fn test_resolve_execution_order() {
        let sections = vec![
            Section {
                title: "format".to_string(),
                codes: vec![],
                description: None,
                depends: vec![],
                params: vec![],
                aliases: vec![],
                private: false,
                env: vec![],
                dir: None,
                ..Default::default()
            },
            Section {
                title: "lint".to_string(),
                codes: vec![],
                description: None,
                depends: vec!["format".to_string()],
                params: vec![],
                aliases: vec![],
                private: false,
                env: vec![],
                dir: None,
                ..Default::default()
            },
            Section {
                title: "test".to_string(),
                codes: vec![],
                description: None,
                depends: vec!["lint".to_string()],
                params: vec![],
                aliases: vec![],
                private: false,
                env: vec![],
                dir: None,
                ..Default::default()
            },
        ];

        let runner = Runner::with_default_config();
        let order = runner.resolve_execution_order(&sections, "test").unwrap();

        assert_eq!(order.len(), 3);
        assert_eq!(order[0].title, "format");
        assert_eq!(order[1].title, "lint");
        assert_eq!(order[2].title, "test");
    }

    #[test]
    fn test_circular_dependency_detected() {
        let sections = vec![
            Section {
                title: "a".to_string(),
                codes: vec![],
                description: None,
                depends: vec!["b".to_string()],
                params: vec![],
                aliases: vec![],
                private: false,
                env: vec![],
                dir: None,
                ..Default::default()
            },
            Section {
                title: "b".to_string(),
                codes: vec![],
                description: None,
                depends: vec!["a".to_string()],
                params: vec![],
                aliases: vec![],
                private: false,
                env: vec![],
                dir: None,
                ..Default::default()
            },
        ];

        let runner = Runner::with_default_config();
        let result = runner.resolve_execution_order(&sections, "a");
        assert!(matches!(result, Err(Error::CircularDependency(_))));
    }

    #[test]
    fn test_shared_dependency_runs_once() {
        let sections = vec![
            Section {
                title: "format".to_string(),
                codes: vec![],
                description: None,
                depends: vec![],
                params: vec![],
                aliases: vec![],
                private: false,
                env: vec![],
                dir: None,
                ..Default::default()
            },
            Section {
                title: "lint".to_string(),
                codes: vec![],
                description: None,
                depends: vec!["format".to_string()],
                params: vec![],
                aliases: vec![],
                private: false,
                env: vec![],
                dir: None,
                ..Default::default()
            },
            Section {
                title: "test".to_string(),
                codes: vec![],
                description: None,
                depends: vec!["format".to_string(), "lint".to_string()],
                params: vec![],
                aliases: vec![],
                private: false,
                env: vec![],
                dir: None,
                ..Default::default()
            },
        ];

        let runner = Runner::with_default_config();
        let order = runner.resolve_execution_order(&sections, "test").unwrap();

        // format should appear only once even though both test and lint depend on it
        assert_eq!(order.len(), 3);
        assert_eq!(order[0].title, "format");
        assert_eq!(order[1].title, "lint");
        assert_eq!(order[2].title, "test");
    }

    #[test]
    fn test_params_parsed_from_meta_block() {
        let markdown = r#"# Title

## deploy

```meta
params = ["env=staging", "verbose"]
```

```bash
echo "deploying"
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();

        let deploy = sections.iter().find(|s| s.title == "deploy").unwrap();
        assert_eq!(deploy.params.len(), 2);
        assert_eq!(deploy.params[0].name, "env");
        assert_eq!(deploy.params[0].default, Some("staging".to_string()));
        assert_eq!(deploy.params[1].name, "verbose");
        assert_eq!(deploy.params[1].default, None);
    }

    #[test]
    fn test_env_parsed_from_meta_block() {
        let markdown = r#"# Title

## deploy

```meta
env = ["REGION=eu", "DEBUG=1"]
```

```bash
echo "deploying"
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();

        let deploy = sections.iter().find(|s| s.title == "deploy").unwrap();
        assert_eq!(
            deploy.env,
            vec![
                ("REGION".to_string(), "eu".to_string()),
                ("DEBUG".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn test_cli_env_override_takes_precedence_over_meta_env() {
        let section = Section {
            title: "deploy".to_string(),
            env: vec![("REGION".to_string(), "staging".to_string())],
            codes: vec![],
            ..Default::default()
        };

        let mut runner = Runner::with_default_config();
        runner.set_env_overrides(vec![("REGION".to_string(), "prod".to_string())]);

        // Task-level env is bound first; prepare_env_vars appends CLI overrides
        // last, so the same key from --env wins.
        let mut task_env = section.env.clone();
        task_env.extend(Runner::bind_params(&section.params, &[], &section.title).unwrap());
        let env_vars = runner.prepare_env_vars(&[], &task_env);

        let region_values: Vec<_> = env_vars
            .iter()
            .filter(|(k, _)| k == "REGION")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(region_values, vec!["staging", "prod"]);
        // std::process::Command::envs applies later entries last, so "prod" wins.
    }

    #[test]
    fn test_dir_parsed_from_meta_block() {
        let markdown = r#"# Title

## deploy

```meta
dir = "services/api"
```

```bash
echo "deploying"
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();

        let deploy = sections.iter().find(|s| s.title == "deploy").unwrap();
        assert_eq!(deploy.dir, Some(PathBuf::from("services/api")));
    }

    #[test]
    fn test_cli_dir_override_takes_precedence_over_meta_dir() {
        let section = Section {
            title: "deploy".to_string(),
            dir: Some(PathBuf::from("services/api")),
            codes: vec![],
            ..Default::default()
        };

        // No CLI --dir: meta dir is used.
        let runner = Runner::with_default_config();
        let resolved = runner
            .exec
            .working_dir
            .as_deref()
            .or(section.dir.as_deref());
        assert_eq!(resolved, Some(Path::new("services/api")));

        // CLI --dir set: it wins over the meta default.
        let mut runner = Runner::with_default_config();
        runner.set_working_dir(Some(PathBuf::from("/tmp/override")));
        let resolved = runner
            .exec
            .working_dir
            .as_deref()
            .or(section.dir.as_deref());
        assert_eq!(resolved, Some(Path::new("/tmp/override")));
    }

    #[test]
    fn test_bind_params_uses_named_positional_and_default() {
        let params = vec![
            ParamDef {
                name: "env".to_string(),
                default: Some("staging".to_string()),
                ..Default::default()
            },
            ParamDef {
                name: "region".to_string(),
                default: None,
                ..Default::default()
            },
        ];

        // named + positional
        let bound = Runner::bind_params(
            &params,
            &["region=eu".to_string(), "prod".to_string()],
            "deploy",
        )
        .unwrap();
        assert_eq!(
            bound,
            vec![
                ("MX_PARAM_ENV".to_string(), "prod".to_string()),
                ("MX_PARAM_REGION".to_string(), "eu".to_string()),
            ]
        );

        // falls back to default when unset
        let bound = Runner::bind_params(&params, &["region=eu".to_string()], "deploy").unwrap();
        assert_eq!(
            bound[0],
            ("MX_PARAM_ENV".to_string(), "staging".to_string())
        );

        // missing required param is an error
        let result = Runner::bind_params(&params, &[], "deploy");
        assert!(matches!(result, Err(Error::MissingParameter(_, _))));
    }

    #[test]
    fn test_parse_env_overrides() {
        let parsed =
            Runner::parse_env_overrides(&["REGION=eu".to_string(), "DEBUG=1".to_string()]).unwrap();
        assert_eq!(
            parsed,
            vec![
                ("REGION".to_string(), "eu".to_string()),
                ("DEBUG".to_string(), "1".to_string()),
            ]
        );

        // value may contain '='
        let parsed = Runner::parse_env_overrides(&["URL=https://a.b?c=d".to_string()]).unwrap();
        assert_eq!(
            parsed,
            vec![("URL".to_string(), "https://a.b?c=d".to_string())]
        );

        // missing '=' is an error
        let result = Runner::parse_env_overrides(&["INVALID".to_string()]);
        assert!(matches!(result, Err(Error::InvalidEnv(_))));

        // empty key is an error
        let result = Runner::parse_env_overrides(&["=value".to_string()]);
        assert!(matches!(result, Err(Error::InvalidEnv(_))));
    }

    #[test]
    fn test_global_env_parsed_from_preamble_meta_block() {
        let markdown = r#"```meta
env = ["REGION=eu", "LOG_LEVEL=info"]
```

# Title

## deploy

```bash
echo "deploying"
```
"#;

        let mut runner = Runner::with_default_config();
        runner.extract_sections(markdown).unwrap();

        assert_eq!(
            runner.exec.global_env,
            vec![
                ("REGION".to_string(), "eu".to_string()),
                ("LOG_LEVEL".to_string(), "info".to_string()),
            ]
        );
    }

    #[test]
    fn test_global_dir_parsed_from_preamble_meta_block() {
        let markdown = r#"```meta
dir = "services/api"
```

# Title

## deploy

```bash
pwd
```
"#;

        let mut runner = Runner::with_default_config();
        runner.extract_sections(markdown).unwrap();

        assert_eq!(runner.exec.global_dir, Some(PathBuf::from("services/api")));
    }

    #[test]
    fn test_no_preamble_meta_leaves_global_defaults_empty() {
        let markdown = r#"# Title

## deploy

```bash
echo "deploying"
```
"#;

        let mut runner = Runner::with_default_config();
        runner.extract_sections(markdown).unwrap();

        assert!(runner.exec.global_env.is_empty());
        assert!(runner.exec.global_dir.is_none());
    }

    #[test]
    fn test_task_meta_env_overrides_global_env() {
        let markdown = r#"```meta
env = ["REGION=eu", "LOG_LEVEL=info"]
```

# Title

## deploy

```meta
env = ["REGION=staging"]
```

```bash
echo "deploying"
```
"#;

        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(markdown).unwrap();
        let deploy = sections.iter().find(|s| s.title == "deploy").unwrap();

        // Global env is merged in ahead of the task's own env; the task's
        // value for a shared key wins because later `Command::envs` entries
        // override earlier ones with the same key.
        let mut task_env = runner.exec.global_env.clone();
        task_env.extend(deploy.env.clone());

        let region_values: Vec<_> = task_env
            .iter()
            .filter(|(k, _)| k == "REGION")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(region_values, vec!["eu", "staging"]);

        // LOG_LEVEL only comes from the global default.
        assert!(task_env.contains(&("LOG_LEVEL".to_string(), "info".to_string())));
    }

    #[test]
    fn test_task_dir_overrides_global_dir() {
        let section = Section {
            title: "deploy".to_string(),
            dir: Some(PathBuf::from("services/api")),
            codes: vec![],
            ..Default::default()
        };

        let mut runner = Runner::with_default_config();
        runner.exec.global_dir = Some(PathBuf::from("services/default"));

        // Task's own dir wins over the global default.
        let resolved = runner
            .exec
            .working_dir
            .as_deref()
            .or(section.dir.as_deref())
            .or(runner.exec.global_dir.as_deref());
        assert_eq!(resolved, Some(Path::new("services/api")));

        // Falls back to the global default when the task declares none.
        let section_no_dir = Section {
            title: "build".to_string(),
            codes: vec![],
            ..Default::default()
        };
        let resolved = runner
            .exec
            .working_dir
            .as_deref()
            .or(section_no_dir.dir.as_deref())
            .or(runner.exec.global_dir.as_deref());
        assert_eq!(resolved, Some(Path::new("services/default")));
    }

    #[test]
    fn test_env_overrides_included_in_prepared_env() {
        let mut runner = Runner::with_default_config();
        runner.set_env_overrides(vec![("REGION".to_string(), "eu".to_string())]);

        let env_vars = runner.prepare_env_vars(&["arg1".to_string()], &[]);
        assert!(env_vars.contains(&("REGION".to_string(), "eu".to_string())));
        assert!(env_vars.contains(&("MX_ARGS".to_string(), "arg1".to_string())));
    }

    #[rstest::rstest]
    #[case("env", "env", None)]
    #[case("  env  ", "env", None)]
    #[case("env=staging", "env", Some("staging"))]
    #[case(" env = staging ", "env", Some("staging"))]
    #[case("env=", "env", Some(""))]
    #[case("url=http://x?a=b", "url", Some("http://x?a=b"))]
    #[case("=v", "", Some("v"))]
    #[case("", "", None)]
    fn param_def_parse(#[case] raw: &str, #[case] name: &str, #[case] default: Option<&str>) {
        let p = ParamDef::parse(raw);
        assert_eq!(p.name, name);
        assert_eq!(p.default.as_deref(), default);
    }

    proptest::proptest! {
        #[test]
        fn param_def_parse_round_trips(
            name in "[a-z][a-z0-9_]{0,8}",
            default in proptest::option::of("[a-zA-Z0-9_./:=-]{0,10}"),
        ) {
            let raw = match &default {
                Some(d) => format!("{name}={d}"),
                None => name.clone(),
            };
            let p = ParamDef::parse(&raw);
            proptest::prop_assert_eq!(p.name, name);
            proptest::prop_assert_eq!(p.default, default);
        }
    }

    fn named(title: &str, depends: &[&str], aliases: &[&str]) -> Section {
        Section {
            title: title.into(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn namespacing_qualifies_titles_aliases_and_local_deps_only() {
        let mut sections = vec![
            named("build", &[], &["b"]),
            named("test", &["build", "b", "elsewhere:lint", "ghost"], &[]),
        ];
        namespace_sections(&mut sections, "lib");
        assert_eq!(sections[0].title, "lib:build");
        assert_eq!(sections[0].aliases, ["lib:b"]);
        assert_eq!(sections[1].title, "lib:test");
        // A local title or alias is qualified; anything else is left alone.
        assert_eq!(
            sections[1].depends,
            ["lib:build", "lib:b", "elsewhere:lint", "ghost"]
        );
    }

    #[test]
    fn namespacing_is_a_noop_on_empty_input() {
        let mut sections: Vec<Section> = Vec::new();
        namespace_sections(&mut sections, "x");
        assert!(sections.is_empty());
    }

    proptest::proptest! {
        #[test]
        fn namespaced_titles_are_unique_and_prefixed(
            titles in proptest::collection::btree_set("[a-z]{1,6}", 1..8),
        ) {
            let mut sections: Vec<Section> = titles.iter().map(|t| named(t, &[], &[])).collect();
            // Every task depends on the first one.
            let first = sections[0].title.clone();
            for s in sections.iter_mut().skip(1) {
                s.depends.push(first.clone());
            }
            namespace_sections(&mut sections, "ns");
            for s in &sections {
                proptest::prop_assert!(s.title.starts_with("ns:"));
                for d in &s.depends {
                    proptest::prop_assert_eq!(d, &format!("ns:{}", titles.iter().next().unwrap()));
                }
            }
            // Qualified dependencies still resolve inside the namespaced set.
            for s in &sections {
                for d in &s.depends {
                    proptest::prop_assert!(schedule::find_section(&sections, d).is_some());
                }
            }
        }
    }

    #[test]
    fn globals_are_prepended_to_task_env_and_fill_missing_dir() {
        let globals = Globals {
            env: vec![("G".into(), "1".into()), ("K".into(), "global".into())],
            dir: Some(PathBuf::from("gdir")),
            imports: Vec::new(),
            dotenv: vec![".env".into()],
            vars: Vec::new(),
        };
        let mut with_dir = named("a", &[], &[]);
        with_dir.env = vec![("K".into(), "task".into())];
        with_dir.dir = Some(PathBuf::from("own"));
        let without_dir = named("b", &[], &[]);

        let mut sections = vec![with_dir, without_dir];
        apply_globals(&mut sections, &globals);

        // Task entries come last, so they win when applied in order.
        assert_eq!(
            sections[0].env,
            [
                ("G".to_string(), "1".to_string()),
                ("K".to_string(), "global".to_string()),
                ("K".to_string(), "task".to_string()),
            ]
        );
        assert_eq!(sections[0].dir, Some(PathBuf::from("own")));
        assert_eq!(sections[1].dir, Some(PathBuf::from("gdir")));
        assert_eq!(sections[1].env.len(), 2);
    }

    fn parse(markdown: &str) -> Result<Vec<Section>> {
        Runner::with_default_config().extract_sections(markdown)
    }

    #[test]
    fn parses_loop_specs() {
        let md = r#"## list

```meta
for = ["a", 2, true]
as = "thing"
```

```bash
true
```

## globbed

```meta
for = { glob = ["a/*.md", "b/*.md"] }
```

```bash
true
```

## matrix

```meta
for = { matrix = { os = ["linux", "mac"], arch = ["x64"] } }
```

```bash
true
```
"#;
        let sections = parse(md).unwrap();
        assert_eq!(
            sections[0].for_each,
            Some(LoopSpec::List(vec!["a".into(), "2".into(), "true".into()]))
        );
        assert_eq!(sections[0].loop_var.as_deref(), Some("thing"));
        assert_eq!(
            sections[1].for_each,
            Some(LoopSpec::Glob(vec!["a/*.md".into(), "b/*.md".into()]))
        );
        assert_eq!(
            sections[2].for_each,
            Some(LoopSpec::Matrix(vec![
                ("arch".into(), vec!["x64".into()]),
                ("os".into(), vec!["linux".into(), "mac".into()]),
            ]))
        );
    }

    #[test]
    fn rejects_invalid_loops() {
        let wrap = |meta: &str| format!("## t\n\n```meta\n{}\n```\n\n```bash\ntrue\n```\n", meta);
        for meta in [
            "for = \"abc\"",
            "for = { nope = [\"a\"] }",
            "for = { glob = \"a\", matrix = {} }",
            "for = [[\"nested\"]]",
            "for = { matrix = [\"a\"] }",
            "as = \"x\"",
        ] {
            let err = parse(&wrap(meta)).unwrap_err();
            assert!(matches!(err, Error::Invalid(_)), "{}: {:?}", meta, err);
        }
    }

    #[test]
    fn parses_defer() {
        let md = "## t\n\n```meta\ndefer = [\"cleanup\", \"notify status=failed\"]\n```\n\n```bash\ntrue\n```\n";
        let sections = parse(md).unwrap();
        assert_eq!(sections[0].defer, vec!["cleanup", "notify status=failed"]);
    }

    #[test]
    fn namespacing_qualifies_calls_with_arguments() {
        let mut sections = vec![
            Section {
                title: "build".into(),
                ..Default::default()
            },
            Section {
                title: "Build App".into(),
                ..Default::default()
            },
            Section {
                title: "ci".into(),
                depends: vec![
                    "build env=prod".into(),
                    "Build App x=1".into(),
                    "other:task a=b".into(),
                    "build".into(),
                ],
                defer: vec!["build 'a b'".into()],
                ..Default::default()
            },
        ];
        namespace_sections(&mut sections, "ns");
        assert_eq!(
            sections[2].depends,
            vec![
                "ns:build env=prod",
                "ns:Build App x=1",
                "other:task a=b",
                "ns:build"
            ]
        );
        assert_eq!(sections[2].defer, vec!["ns:build \"a b\""]);
        let found = schedule::resolve_call(&sections, &sections[2].depends[1]).unwrap();
        assert_eq!(found.0.title, "ns:Build App");
        assert_eq!(found.1, vec!["x=1"]);
    }

    #[test]
    fn no_run_blocks_and_data_languages_are_not_commands() {
        let md = "## build\n\n```bash no-run\necho skipped\n```\n\n```bash\necho run\n```\n\n```json\n{}\n```\n\n```text\nout\n```\n";
        let sections = parse(md).unwrap();
        let langs: Vec<_> = sections[0].codes.iter().map(|c| c.lang.as_str()).collect();
        assert_eq!(langs, ["bash"]);
        assert_eq!(sections[0].codes[0].code, "echo run");
    }

    #[test]
    fn a_data_language_with_a_runtime_still_runs() {
        let mut config = Config::default();
        config.runtimes.insert(
            "json".to_string(),
            crate::config::RuntimeConfig::Simple("jq .".to_string()),
        );
        let mut runner = Runner::new(config);
        let sections = runner
            .extract_sections("## q\n\n```json\n{}\n```\n")
            .unwrap();
        assert_eq!(sections[0].codes.len(), 1);
    }

    #[test]
    fn params_may_be_tables_with_choices_and_a_description() {
        let md = "## deploy\n\n```meta\nparams = [\n  \"tag=latest\",\n  { name = \"env\", default = \"dev\", choices = [\"dev\", \"prod\"], desc = \"where to deploy\" },\n  { name = \"region\" },\n]\n```\n\n```bash\ntrue\n```\n";
        let params = parse(md).unwrap().remove(0).params;
        assert_eq!(params.len(), 3);
        assert_eq!(
            (params[0].name.as_str(), params[0].default.as_deref()),
            ("tag", Some("latest"))
        );
        assert_eq!(params[1].choices, ["dev", "prod"]);
        assert_eq!(params[1].description.as_deref(), Some("where to deploy"));
        assert_eq!(params[2].default, None);
        assert_eq!(params[1].describe(), "env=dev [dev|prod] - where to deploy");
        assert_eq!(params[2].describe(), "region (required)");
    }

    #[rstest::rstest]
    #[case::unknown_key(
        "{ name = \"a\", chocies = [\"x\"] }",
        "unknown key `chocies` (did you mean `choices`?)"
    )]
    #[case::no_name("{ default = \"a\" }", "needs a `name`")]
    #[case::empty_choices("{ name = \"a\", choices = [] }", "`choices` of 'a' is empty")]
    #[case::default_outside(
        "{ name = \"a\", default = \"z\", choices = [\"x\"] }",
        "the default 'z' of 'a' is not one of its choices (x)"
    )]
    #[case::bad_type("42", "expected a string or a table")]
    fn invalid_param_tables_are_explained(#[case] param: &str, #[case] expected: &str) {
        let md = format!(
            "## t\n\n```meta\nparams = [{}]\n```\n\n```bash\ntrue\n```\n",
            param
        );
        let err = parse(&md).unwrap_err().to_string();
        assert!(err.contains("task 't'") && err.contains(expected), "{err}");
    }

    #[test]
    fn parses_ignore_error() {
        let md = "## t\n\n```meta\nignore_error = true\n```\n\n```bash\ntrue\n```\n";
        let sections = parse(md).unwrap();
        assert!(sections[0].ignore_error);
        let md = "## t\n\n```bash\ntrue\n```\n";
        assert!(!parse(md).unwrap()[0].ignore_error);
    }

    #[test]
    fn parses_prompt() {
        let md = "## t\n\n```meta\nprompt = \"Really?\"\n```\n\n```bash\ntrue\n```\n";
        assert_eq!(parse(md).unwrap()[0].prompt.as_deref(), Some("Really?"));
    }

    #[test]
    fn parses_dotenv_in_tasks_and_preamble() {
        let md = "```meta\ndotenv = [\".env\"]\n```\n\n## t\n\n```meta\ndotenv = [\".env.local\"]\n```\n\n```bash\ntrue\n```\n";
        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(md).unwrap();
        assert_eq!(runner.exec.global_dotenv, vec![".env"]);
        assert_eq!(sections[0].dotenv, vec![".env.local"]);
    }

    #[test]
    fn parses_status() {
        let md = "## t\n\n```meta\nstatus = [\"test -f out.txt\", \"true\"]\n```\n\n```bash\ntrue\n```\n";
        assert_eq!(
            parse(md).unwrap()[0].status,
            vec!["test -f out.txt", "true"]
        );
    }

    #[test]
    fn parses_once() {
        let md = "## t\n\n```meta\nonce = true\n```\n\n```bash\ntrue\n```\n";
        assert!(parse(md).unwrap()[0].once);
    }

    #[test]
    fn parses_requires() {
        let md =
            "## t\n\n```meta\nrequires = [\"API_TOKEN\", \"REGION\"]\n```\n\n```bash\ntrue\n```\n";
        assert_eq!(parse(md).unwrap()[0].requires, vec!["API_TOKEN", "REGION"]);
    }

    #[test]
    fn parses_for_from() {
        let md = "## t\n\n```meta\nfor = { from = \" list \" }\n```\n\n```bash\ntrue\n```\n";
        let sections = parse(md).unwrap();
        assert_eq!(sections[0].for_each, Some(LoopSpec::From("list".into())));
        assert_eq!(
            sections[0].for_each.as_ref().unwrap().summary(),
            "output of list"
        );
        for bad in ["for = { from = 3 }", "for = { from = \"\" }"] {
            let md = format!("## t\n\n```meta\n{}\n```\n\n```bash\ntrue\n```\n", bad);
            assert!(matches!(parse(&md), Err(Error::Invalid(_))), "{}", bad);
        }
    }

    #[test]
    fn namespacing_qualifies_the_for_from_source() {
        let mut sections = vec![
            Section {
                title: "list".into(),
                ..Default::default()
            },
            Section {
                title: "each".into(),
                for_each: Some(LoopSpec::From("list".into())),
                ..Default::default()
            },
            Section {
                title: "other".into(),
                for_each: Some(LoopSpec::From("elsewhere:list".into())),
                ..Default::default()
            },
        ];
        namespace_sections(&mut sections, "ns");
        assert_eq!(sections[1].for_each, Some(LoopSpec::From("ns:list".into())));
        assert_eq!(
            sections[2].for_each,
            Some(LoopSpec::From("elsewhere:list".into()))
        );
    }

    #[test]
    fn loop_specs_have_short_summaries() {
        let list = |n: usize| LoopSpec::List((0..n).map(|i| i.to_string()).collect());
        assert_eq!(list(2).summary(), "0, 1");
        assert_eq!(list(5).summary(), "0, 1, 2, ...");
        assert_eq!(
            LoopSpec::Glob(vec!["a/*".into(), "b/*".into()]).summary(),
            "glob a/*, b/*"
        );
        let matrix = LoopSpec::Matrix(vec![("arch".into(), vec![]), ("os".into(), vec![])]);
        assert_eq!(matrix.summary(), "matrix arch x os");
        assert_eq!(LoopSpec::From("ls".into()).summary(), "output of ls");
    }

    #[test]
    fn quote_word_round_trips_through_split_words() {
        for word in ["plain", "a b", "k=v w", "say \"hi\"", "", "x'y"] {
            let quoted = quote_word(word);
            assert_eq!(split_words(&quoted), vec![word.to_string()], "{}", quoted);
        }
    }

    #[test]
    fn matrix_axes_are_sorted_and_scalars_become_strings() {
        let md = "## t\n\n```meta\nfor = { matrix = { b = [1, 2.5], a = [true] } }\n```\n\n```bash\ntrue\n```\n";
        assert_eq!(
            parse(md).unwrap()[0].for_each,
            Some(LoopSpec::Matrix(vec![
                ("a".into(), vec!["true".into()]),
                ("b".into(), vec!["1".into(), "2.5".into()]),
            ]))
        );
    }

    #[test]
    fn a_glob_may_be_one_pattern_or_several() {
        let one = "## t\n\n```meta\nfor = { glob = \"*.md\" }\n```\n\n```bash\ntrue\n```\n";
        assert_eq!(
            parse(one).unwrap()[0].for_each,
            Some(LoopSpec::Glob(vec!["*.md".into()]))
        );
        let bad = "## t\n\n```meta\nfor = { glob = 3 }\n```\n\n```bash\ntrue\n```\n";
        assert!(matches!(parse(bad), Err(Error::Invalid(_))));
    }

    #[test]
    fn unknown_meta_keys_get_a_suggestion_and_a_line() {
        let md = "## flaky\n\n```meta\nretries = 1\nretires = 3\n```\n\n```bash\ntrue\n```\n";
        let Err(Error::Invalid(msg)) = parse(md) else {
            panic!("expected an error");
        };
        assert!(msg.contains("task 'flaky'"), "{}", msg);
        assert!(msg.contains("unknown key `retires` on line 2"), "{}", msg);
        assert!(msg.contains("did you mean `retries`?"), "{}", msg);
    }

    #[test]
    fn unknown_meta_keys_without_a_close_match_point_to_the_reference() {
        let md = "## t\n\n```meta\nfrobnicate = true\n```\n\n```bash\ntrue\n```\n";
        let Err(Error::Invalid(msg)) = parse(md) else {
            panic!("expected an error");
        };
        assert!(msg.contains("unknown key `frobnicate`"), "{}", msg);
        assert!(msg.contains("Meta keys reference"), "{}", msg);
    }

    #[test]
    fn imports_in_a_task_meta_explains_where_it_belongs() {
        let md = "## t\n\n```meta\nimports = [\"a.md\"]\n```\n\n```bash\ntrue\n```\n";
        let Err(Error::Invalid(msg)) = parse(md) else {
            panic!("expected an error");
        };
        assert!(msg.contains("before the first heading"), "{}", msg);
    }

    #[test]
    fn the_document_meta_block_is_checked_too() {
        let md = "```meta\ndotenv = [\".env\"]\ndri = \"x\"\n```\n\n## t\n\n```bash\ntrue\n```\n";
        let Err(Error::Invalid(msg)) = parse(md) else {
            panic!("expected an error");
        };
        assert!(msg.starts_with("document `meta` block"), "{}", msg);
        assert!(msg.contains("did you mean `dir`?"), "{}", msg);
    }

    #[test]
    fn type_errors_keep_the_original_message() {
        let md = "## t\n\n```meta\nretries = \"many\"\n```\n\n```bash\ntrue\n```\n";
        let Err(Error::Invalid(msg)) = parse(md) else {
            panic!("expected an error");
        };
        assert!(
            msg.contains("task 't'") && msg.contains("invalid type"),
            "{}",
            msg
        );
    }

    #[test]
    fn unknown_field_messages_are_taken_apart() {
        assert_eq!(
            unknown_field("unknown field `a`, expected one of `b`, `c`, `d`"),
            Some(("a".into(), vec!["b".into(), "c".into(), "d".into()]))
        );
        assert_eq!(
            unknown_field("unknown field `a`, expected `b` or `c`"),
            Some(("a".into(), vec!["b".into(), "c".into()]))
        );
        assert_eq!(
            unknown_field("unknown field `a`, expected `b`").unwrap().1,
            vec!["b"]
        );
        assert_eq!(unknown_field("invalid type: string"), None);
    }

    #[test]
    fn parses_vars_in_tasks_and_the_preamble() {
        let md = "```meta\nvars = { A = \"1\", B = { sh = \"echo b\" } }\n```\n\n## t\n\n```meta\nvars = { C = 3 }\n```\n\n```bash\ntrue\n```\n";
        let mut runner = Runner::with_default_config();
        let sections = runner.extract_sections(md).unwrap();
        let names = |vars: &[VarDef]| vars.iter().map(|v| v.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&runner.exec.global_vars), vec!["A", "B"]);
        assert_eq!(
            sections[0].vars[0].value,
            crate::vars::VarValue::Literal("3".into())
        );
    }

    #[test]
    fn invalid_vars_name_the_task_and_the_variable() {
        let md = "## t\n\n```meta\nvars = { \"a-b\" = 1 }\n```\n\n```bash\ntrue\n```\n";
        let Err(Error::Invalid(msg)) = parse(md) else {
            panic!("expected an error");
        };
        assert!(
            msg.contains("task 't'") && msg.contains("var `a-b`"),
            "{}",
            msg
        );

        let md = "```meta\nvars = { A = [1] }\n```\n\n## t\n\n```bash\ntrue\n```\n";
        let Err(Error::Invalid(msg)) = parse(md) else {
            panic!("expected an error");
        };
        assert!(msg.starts_with("document `meta` block"), "{}", msg);
    }

    #[test]
    fn imported_files_bring_their_document_vars_along() {
        let globals = Globals {
            env: Vec::new(),
            dir: None,
            imports: Vec::new(),
            dotenv: Vec::new(),
            vars: vec![crate::vars::VarDef {
                name: "FROM_FILE".into(),
                value: crate::vars::VarValue::Literal("1".into()),
            }],
        };
        let mut own = named("a", &[], &[]);
        own.vars = vec![crate::vars::VarDef {
            name: "OWN".into(),
            value: crate::vars::VarValue::Literal("2".into()),
        }];
        let mut sections = vec![own];
        apply_globals(&mut sections, &globals);
        let names: Vec<&str> = sections[0].vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["FROM_FILE", "OWN"]);
    }
}
