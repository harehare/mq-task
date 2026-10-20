//! The task-file document model: sections, code blocks, `meta` blocks and the
//! pieces they are parsed into.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::RuntimeConfig;
use crate::error::{Error, Result};
use crate::vars::VarDef;

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
    pub(crate) fn from_toml(title: &str, value: &toml::Value) -> Result<Self> {
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
pub(crate) struct GlobalMeta {
    /// Default environment variables applied to every task
    #[serde(default)]
    pub(crate) env: Vec<String>,
    /// Default working directory applied to every task without its own `dir`
    #[serde(default)]
    pub(crate) dir: Option<String>,
    /// Other task files whose tasks are made available as `<name>:<task>`:
    /// paths, globs or `{ path, as, optional, flatten }` tables
    #[serde(default)]
    pub(crate) imports: Vec<toml::Value>,
    /// `.env` files loaded for every task
    #[serde(default)]
    pub(crate) dotenv: Vec<String>,
    /// Variables available to every task
    #[serde(default)]
    pub(crate) vars: Option<toml::Table>,
    /// Runtimes for this file's languages, in the form of `[runtimes]` in
    /// `mq-task.toml`
    #[serde(default)]
    pub(crate) runtimes: Option<toml::Table>,
    /// Task run when no task is named
    #[serde(default)]
    pub(crate) default_task: Option<String>,
}

/// Task metadata parsed from a `meta` code block within a section
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskMeta {
    #[serde(default)]
    pub(crate) depends: Vec<String>,
    #[serde(default)]
    pub(crate) params: Vec<toml::Value>,
    #[serde(default)]
    pub(crate) alias: Vec<String>,
    #[serde(default)]
    pub(crate) private: bool,
    /// Default environment variables, e.g. `env = ["REGION=eu", "DEBUG=1"]`
    #[serde(default)]
    pub(crate) env: Vec<String>,
    /// Working directory to run this task's commands in, e.g. `dir = "services/api"`
    #[serde(default)]
    pub(crate) dir: Option<String>,
    /// Run this task's dependencies concurrently
    #[serde(default)]
    pub(crate) parallel: bool,
    /// Condition that must hold for the task to run
    #[serde(default, rename = "if")]
    pub(crate) condition: Option<String>,
    /// How many times to retry after a failure
    #[serde(default)]
    pub(crate) retries: u32,
    /// Delay between retries, e.g. `"2s"`
    #[serde(default)]
    pub(crate) retry_delay: Option<String>,
    /// Kill the task if it runs longer than this, e.g. `"30s"`
    #[serde(default)]
    pub(crate) timeout: Option<String>,
    /// Expose this task's stdout to dependents as `MX_OUTPUT_<TASK>`
    #[serde(default)]
    pub(crate) capture: bool,
    /// Globs of input files; unchanged inputs let the task be skipped
    #[serde(default)]
    pub(crate) sources: Vec<String>,
    /// Globs of files the task produces; must exist for it to be skipped
    #[serde(default)]
    pub(crate) outputs: Vec<String>,
    /// Run the task once per item: an array, `{ glob = ... }` or `{ matrix = ... }`
    #[serde(default, rename = "for")]
    pub(crate) for_each: Option<toml::Value>,
    /// Extra environment variable name for the loop item, e.g. `as = "file"`
    #[serde(default, rename = "as")]
    pub(crate) loop_var: Option<String>,
    /// Tasks run after this one finishes, even if it failed
    #[serde(default)]
    pub(crate) defer: Vec<String>,
    /// Treat a failed run (after retries) as success
    #[serde(default)]
    pub(crate) ignore_error: bool,
    /// Ask for confirmation before running, e.g. `prompt = "Deploy to production?"`
    #[serde(default)]
    pub(crate) prompt: Option<String>,
    /// `.env` files loaded for this task, e.g. `dotenv = [".env", ".env.local"]`
    #[serde(default)]
    pub(crate) dotenv: Vec<String>,
    /// Shell commands; the task is up to date when all of them exit 0
    #[serde(default)]
    pub(crate) status: Vec<String>,
    /// Variables: fixed values or `{ sh = "command" }`
    #[serde(default)]
    pub(crate) vars: Option<toml::Table>,
    /// Run only once per run, however it is called
    #[serde(default)]
    pub(crate) once: bool,
    /// Variables that must be set (and not empty), e.g. `["API_TOKEN"]`
    #[serde(default)]
    pub(crate) requires: Vec<String>,
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
    pub(crate) fn parse(raw: &str) -> Self {
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
    pub(crate) fn from_toml(task: &str, value: &toml::Value) -> Result<Self> {
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

/// One entry of `imports`: a path (or glob) to a task file, with the options
/// of the table form.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImportSpec {
    pub(crate) path: String,
    /// Namespace to use instead of the file name.
    pub(crate) alias: Option<String>,
    /// A missing file is not an error.
    pub(crate) optional: bool,
    /// Add the tasks under their own names, without a namespace.
    pub(crate) flatten: bool,
}

impl ImportSpec {
    pub(crate) fn is_glob(&self) -> bool {
        self.path.contains(['*', '?', '['])
    }

    pub(crate) fn from_toml(value: &toml::Value) -> Result<Self> {
        let invalid =
            |msg: &str| Error::Invalid(format!("document `meta` block: `imports`: {}", msg));
        let spec = match value {
            toml::Value::String(path) => ImportSpec {
                path: path.clone(),
                alias: None,
                optional: false,
                flatten: false,
            },
            toml::Value::Table(table) => {
                const KEYS: [&str; 4] = ["path", "as", "optional", "flatten"];
                for key in table.keys() {
                    if !KEYS.contains(&key.as_str()) {
                        let hint = crate::util::closest(key, KEYS)
                            .map(|c| format!(" (did you mean `{}`?)", c))
                            .unwrap_or_else(|| {
                                " (expected `path`, `as`, `optional` or `flatten`)".into()
                            });
                        return Err(invalid(&format!("unknown key `{}`{}", key, hint)));
                    }
                }
                let text = |key: &str| match table.get(key) {
                    None => Ok(None),
                    Some(toml::Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
                    Some(_) => Err(invalid(&format!("`{}` must be a non-empty string", key))),
                };
                let flag = |key: &str| match table.get(key) {
                    None => Ok(false),
                    Some(toml::Value::Boolean(b)) => Ok(*b),
                    Some(_) => Err(invalid(&format!("`{}` must be true or false", key))),
                };
                ImportSpec {
                    path: text("path")?.ok_or_else(|| invalid("a table needs a `path`"))?,
                    alias: text("as")?,
                    optional: flag("optional")?,
                    flatten: flag("flatten")?,
                }
            }
            _ => return Err(invalid("expected a path or a table")),
        };
        if spec.is_glob() && spec.alias.is_some() {
            return Err(invalid(&format!(
                "`as` cannot be used with the glob '{}': every file is named after itself",
                spec.path
            )));
        }
        if spec.flatten && spec.alias.is_some() {
            return Err(invalid(&format!(
                "`as` has no effect with `flatten` ('{}')",
                spec.path
            )));
        }
        Ok(spec)
    }

    /// The files this entry stands for under `base`, each with its
    /// namespace. `current` (the importing file) is never matched by a glob.
    pub(crate) fn resolve(&self, base: &Path, current: &Path) -> Result<Vec<(PathBuf, String)>> {
        let namespace = |path: &Path| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .ok_or_else(|| Error::Invalid(format!("invalid import path '{}'", self.path)))
        };
        if self.is_glob() {
            let pattern = base.join(&self.path).to_string_lossy().into_owned();
            let paths = glob::glob(&pattern).map_err(|e| {
                Error::Invalid(format!("invalid import pattern '{}': {}", self.path, e.msg))
            })?;
            let mut found: Vec<PathBuf> = paths
                .flatten()
                .filter(|p| p.is_file() && p.canonicalize().ok().as_deref() != Some(current))
                .collect();
            found.sort();
            return found
                .into_iter()
                .map(|p| namespace(&p).map(|ns| (p, ns)))
                .collect();
        }
        let path = base.join(&self.path);
        if self.optional && !path.exists() {
            return Ok(Vec::new());
        }
        let namespace = match &self.alias {
            Some(alias) => alias.clone(),
            None => namespace(&path)?,
        };
        Ok(vec![(path, namespace)])
    }
}

/// Document-wide settings found in the preamble `meta` block.
#[derive(Debug, Default)]
pub(crate) struct Globals {
    pub(crate) env: Vec<(String, String)>,
    pub(crate) dir: Option<PathBuf>,
    pub(crate) imports: Vec<ImportSpec>,
    pub(crate) dotenv: Vec<String>,
    pub(crate) vars: Vec<VarDef>,
    pub(crate) runtimes: Vec<(String, RuntimeConfig)>,
    pub(crate) default_task: Option<String>,
}
