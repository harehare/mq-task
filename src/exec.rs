//! Process execution: runs a task's code blocks, including the per-task
//! lifecycle (`if`, up-to-date checks, retries, timeout, output capture).
//!
//! [`Executor`] holds no mq engine state, so it is `Sync` and can be shared
//! by the worker threads of a parallel run.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cache;
use crate::condition::{self, CondContext};
use crate::config::{Config, ExecutionMode};
use crate::error::{Error, Result};
use crate::events::{NullObserver, RunObserver, Stream, TaskStatus};
use crate::runner::{ParamDef, Section};
use crate::schedule::{Deferred, loop_item_vars};
use crate::util::closest;
use crate::vars::{self, ShellCache, VarDef};

/// Maps a signal-killed process to the shell convention `128 + signal`.
fn exit_code_from_status(status: &ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(1)
}

/// How a task's process I/O is wired for one execution.
#[derive(Clone, Copy)]
pub struct TaskIo<'a> {
    /// Name used when reporting output.
    pub name: &'a str,
    pub observer: &'a dyn RunObserver,
    /// Route stdout/stderr through the observer (parallel runs, TUI) instead
    /// of inheriting the terminal; stdin is then closed.
    pub piped: bool,
    /// Collect stdout so dependents can read it as `MX_OUTPUT_<TASK>`.
    pub capture: bool,
    /// Set to abort the running process (fail-fast).
    pub cancel: &'a AtomicBool,
    /// Kill the process if it is still running at this instant.
    pub deadline: Option<Instant>,
}

/// Result of running a task through its full lifecycle.
#[derive(Debug, PartialEq)]
pub enum NodeOutcome {
    /// Ran to completion; `output` is the captured stdout, if requested.
    Done { output: Option<String> },
    /// Not run (`if` was false, or up to date).
    Skipped,
}

/// Inputs for [`Executor::run_node`].
pub struct NodeParams<'a> {
    pub section: &'a Section,
    /// Name this run is reported and cached under (the plan node's label).
    pub label: &'a str,
    pub args: &'a [String],
    pub lang_filter: Option<&'a str>,
    /// Extra env: `MX_OUTPUT_*` from dependencies and loop variables.
    pub extra_env: Vec<(String, String)>,
    pub dependency: bool,
    /// Dependency whose captured output is looped over (`for.from`).
    pub loop_source: Option<&'a str>,
    /// Tasks to run afterwards, whatever the outcome.
    pub defers: &'a [Deferred<'a>],
}

/// The non-empty lines of `source`'s captured output in `env`.
fn source_items(env: &[(String, String)], source: &str) -> Vec<String> {
    let var = output_var(source);
    env.iter()
        .rev()
        .find(|(k, _)| *k == var)
        .map(|(_, v)| {
            v.lines()
                .map(|l| l.trim_end_matches('\r'))
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Print the code about to run (`-v`) on stderr, through the observer so
/// it follows `--output` and shows in the TUI log.
fn show_block(lang: &str, code: &str, dir: Option<&Path>, io: &TaskIo) {
    let place = dir.map_or(String::new(), |d| format!(" in {}", d.display()));
    io.observer
        .output(io.name, Stream::Stderr, &format!("▸ {}{}", lang, place));
    for line in code.trim_end().lines() {
        let text = if line.trim().is_empty() {
            String::new()
        } else {
            format!("  {}", line)
        };
        io.observer.output(io.name, Stream::Stderr, &text);
    }
}

/// Name of the env var that exposes a captured task's stdout to dependents.
pub fn output_var(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("MX_OUTPUT_{}", name)
}

#[derive(Clone)]
pub struct Executor {
    pub config: Config,
    pub dry_run: bool,
    pub env_overrides: Vec<(String, String)>,
    pub working_dir: Option<PathBuf>,
    /// Document-wide `meta` defaults of the main file.
    pub global_env: Vec<(String, String)>,
    pub global_dir: Option<PathBuf>,
    /// Document-wide `dotenv` files of the main file.
    pub global_dotenv: Vec<String>,
    /// Document-wide `vars` of the main file.
    pub global_vars: Vec<VarDef>,
    /// Shell variables already evaluated in this run.
    var_cache: Arc<ShellCache>,
    /// Honor `sources`/`outputs` up-to-date checks.
    pub use_cache: bool,
    /// Distinguishes cache entries of different task files.
    pub cache_scope: String,
    /// Answer yes to `prompt`s without asking.
    pub assume_yes: bool,
    /// Show each code block before it runs (`-v`).
    pub verbose: bool,
    /// Tasks already confirmed in this run, so a loop asks only once.
    confirmed: Arc<Mutex<HashSet<String>>>,
}

impl Executor {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            dry_run: false,
            env_overrides: Vec::new(),
            working_dir: None,
            global_env: Vec::new(),
            global_dir: None,
            global_dotenv: Vec::new(),
            global_vars: Vec::new(),
            var_cache: Arc::default(),
            use_cache: true,
            cache_scope: String::new(),
            assume_yes: false,
            verbose: false,
            confirmed: Arc::default(),
        }
    }

    /// Working directory for a task: CLI `--dir`, then its own `dir`, then
    /// the document-wide default.
    pub fn effective_dir<'a>(&'a self, section: &'a Section) -> Option<&'a Path> {
        self.working_dir
            .as_deref()
            .or(section.dir.as_deref())
            .or(self.global_dir.as_deref())
    }

    /// Variables from the `dotenv` files; later files win and missing files
    /// are ignored. Variables already in the process environment are kept.
    fn dotenv_vars(&self, section: &Section) -> Result<Vec<(String, String)>> {
        let dir = self.effective_dir(section);
        let mut vars: Vec<(String, String)> = Vec::new();
        for file in self.global_dotenv.iter().chain(&section.dotenv) {
            let path = match dir {
                Some(d) if !Path::new(file).is_absolute() => d.join(file),
                _ => PathBuf::from(file),
            };
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(Error::Invalid(format!("dotenv {}: {}", path.display(), e)));
                }
            };
            let parsed = crate::util::parse_dotenv(&content)
                .map_err(|e| Error::Invalid(format!("dotenv {}: {}", path.display(), e)))?;
            for (key, value) in parsed {
                if std::env::var_os(&key).is_some() {
                    continue;
                }
                vars.retain(|(k, _)| *k != key);
                vars.push((key, value));
            }
        }
        Ok(vars)
    }

    /// Forget shell variables evaluated so far; call before each run so a
    /// re-run (`--watch`) sees fresh values.
    pub fn clear_var_cache(&self) {
        self.var_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// The `vars` that apply to a section: document-wide ones, then its own.
    fn var_defs(&self, section: &Section) -> Vec<VarDef> {
        self.global_vars
            .iter()
            .chain(&section.vars)
            .cloned()
            .collect()
    }

    /// Names of the loop variables that exist only per item (`for.from`)
    /// and of the vars that use them.
    fn item_dependent(&self, section: &Section) -> Vec<String> {
        let roots: Vec<String> = loop_item_vars(section, 0, "")
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let mut names = vars::depending_on(&self.var_defs(section), &roots);
        names.extend(roots);
        names
    }

    /// Names a var cannot replace: set by `env`, `--env` or a bound param.
    fn fixed_names(&self, section: &Section, params: &[(String, String)]) -> Vec<String> {
        self.global_env
            .iter()
            .chain(&section.env)
            .chain(params)
            .chain(&self.env_overrides)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Env for a task before `--env` overrides, in increasing precedence:
    /// `dotenv` files, `vars`, document and task `env`, then bound params.
    /// `extra` (loop variables, dependency outputs) and the params can be
    /// used by `vars`. With `defer_items`, the vars that need the item of a
    /// `for.from` loop are left for [`Self::item_env`]. Computed vars are
    /// stopped by `io`'s cancellation and deadline.
    fn task_env(
        &self,
        section: &Section,
        args: &[String],
        extra: &[(String, String)],
        io: &TaskIo,
        defer_items: bool,
    ) -> Result<Vec<(String, String)>> {
        let dotenv = self.dotenv_vars(section)?;
        let params = Self::bind_params(&section.params, args, &section.title)?;
        let fixed_env: Vec<(String, String)> = self
            .global_env
            .iter()
            .chain(&section.env)
            .cloned()
            .collect();

        let defs = self.var_defs(section);
        let vars = if defs.is_empty() {
            Vec::new()
        } else {
            let mut base = dotenv.clone();
            base.extend(extra.iter().cloned());
            base.extend(fixed_env.iter().cloned());
            base.extend(params.iter().cloned());
            base.extend(self.env_overrides.iter().cloned());
            let deferred = if defer_items {
                self.item_dependent(section)
            } else {
                Vec::new()
            };
            vars::resolve(
                &defs,
                &vars::ResolveContext {
                    base: &base,
                    fixed: &self.fixed_names(section, &params),
                    dir: self.effective_dir(section),
                    dry_run: self.dry_run,
                    cache: &self.var_cache,
                    select: vars::Select::Except(&deferred),
                    io,
                },
            )?
        };

        let mut env = dotenv;
        env.extend(vars);
        env.extend(fixed_env);
        env.extend(params);
        Ok(env)
    }

    /// The vars [`Self::task_env`] left out because they need the item of a
    /// `for.from` loop, evaluated for one item. `task_env` is the env the
    /// task already has and `item` the loop variables of that item.
    fn item_env(
        &self,
        section: &Section,
        task_env: &[(String, String)],
        item: &[(String, String)],
        io: &TaskIo,
    ) -> Result<Vec<(String, String)>> {
        let defs = self.var_defs(section);
        let deferred = vars::depending_on(
            &defs,
            &item.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
        );
        if deferred.is_empty() {
            return Ok(Vec::new());
        }
        let mut base = task_env.to_vec();
        base.extend(item.iter().cloned());
        base.extend(self.env_overrides.iter().cloned());
        vars::resolve(
            &defs,
            &vars::ResolveContext {
                base: &base,
                fixed: &self.fixed_names(section, &[]),
                dir: self.effective_dir(section),
                dry_run: self.dry_run,
                cache: &self.var_cache,
                select: vars::Select::Only(&deferred),
                io,
            },
        )
    }

    /// Bind declared params to CLI args: `name=value` binds by name, the
    /// rest fill remaining params positionally, then defaults. Returns
    /// `MX_PARAM_<NAME>` env pairs; errors if a required param is unbound.
    pub fn bind_params(
        params: &[ParamDef],
        args: &[String],
        task_name: &str,
    ) -> Result<Vec<(String, String)>> {
        if params.is_empty() {
            return Ok(Vec::new());
        }

        let mut named: Vec<(&str, &str)> = Vec::new();
        let mut positional: Vec<&String> = Vec::new();

        for arg in args {
            let stripped = arg.strip_prefix("--").unwrap_or(arg);
            match stripped.split_once('=') {
                Some((key, value)) if params.iter().any(|p| p.name == key) => {
                    named.push((key, value));
                }
                // A word that looks like `name=value` is a named argument, so
                // a misspelt parameter name is an error and not a value.
                Some((key, _)) if is_param_name(key) => {
                    let hint = match closest(key, params.iter().map(|p| p.name.as_str())) {
                        Some(name) => format!(" (did you mean `{}`?)", name),
                        None => format!(
                            " (declared: {})",
                            params
                                .iter()
                                .map(|p| p.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    return Err(Error::UnknownParameter(
                        key.to_string(),
                        task_name.to_string(),
                        hint,
                    ));
                }
                _ => positional.push(arg),
            }
        }

        let mut positional_iter = positional.into_iter();
        let mut env_vars = Vec::new();

        for param in params {
            let value = named
                .iter()
                .find(|(k, _)| *k == param.name)
                .map(|(_, v)| v.to_string())
                .or_else(|| positional_iter.next().cloned())
                .or_else(|| param.default.clone())
                .ok_or_else(|| {
                    Error::MissingParameter(param.name.clone(), task_name.to_string())
                })?;

            env_vars.push((format!("MX_PARAM_{}", param.name.to_uppercase()), value));
        }

        Ok(env_vars)
    }

    /// Every environment variable a task process sees from mq-task, in
    /// increasing precedence: args, task env, individual args, `--env`.
    pub(crate) fn prepare_env_vars(
        &self,
        args: &[String],
        param_env: &[(String, String)],
    ) -> Vec<(String, String)> {
        let mut env_vars = Vec::new();

        if !args.is_empty() {
            env_vars.push(("MX_ARGS".to_string(), args.join(" ")));
        }

        env_vars.extend(param_env.iter().cloned());

        for (i, arg) in args.iter().enumerate() {
            env_vars.push((format!("MX_ARG_{}", i), arg.clone()));
        }

        env_vars.extend(self.env_overrides.iter().cloned());

        env_vars
    }

    /// Run a section's code blocks once, with inherited terminal I/O.
    pub fn execute_section(
        &self,
        section: &Section,
        args: &[String],
        lang_filter: Option<&str>,
    ) -> Result<()> {
        let cancel = AtomicBool::new(false);
        let io = TaskIo {
            name: &section.title,
            observer: &NullObserver,
            piped: false,
            capture: false,
            cancel: &cancel,
            deadline: None,
        };
        let env = self.task_env(section, args, &[], &io, false)?;
        self.run_blocks(section, args, lang_filter, &env, &io)?;
        Ok(())
    }

    /// Run every code block of a section, returning captured stdout when
    /// `io.capture` is set.
    fn run_blocks(
        &self,
        section: &Section,
        args: &[String],
        lang_filter: Option<&str>,
        task_env: &[(String, String)],
        io: &TaskIo,
    ) -> Result<String> {
        let working_dir = self.effective_dir(section);
        let mut captured = String::new();

        for code_block in &section.codes {
            if code_block.lang.is_empty() {
                continue;
            }
            if let Some(filter) = lang_filter
                && code_block.lang != filter
            {
                continue;
            }
            captured.push_str(&self.execute_code_with_params(
                &code_block.lang,
                &code_block.code,
                args,
                task_env,
                working_dir,
                io,
            )?);
        }

        Ok(captured)
    }

    /// Execute one task with its full lifecycle: `if`, up-to-date check,
    /// retries, timeout and deferred tasks.
    pub fn run_node(&self, p: &NodeParams, io: &TaskIo) -> Result<NodeOutcome> {
        let section = p.section;
        let name = p.label;
        let observer = io.observer;

        observer.status(
            name,
            &TaskStatus::Started {
                dependency: p.dependency,
            },
        );

        let fail = |e: Error| {
            let status = match e {
                Error::Cancelled => TaskStatus::Cancelled {
                    reason: "aborted".into(),
                },
                ref other => TaskStatus::Failed {
                    error: other.to_string(),
                },
            };
            observer.status(name, &status);
            e
        };

        // Computed vars run before the task proper, under its timeout.
        let var_io = TaskIo {
            deadline: section.timeout.map(|t| Instant::now() + t),
            ..*io
        };
        let mut task_env = self
            .task_env(
                section,
                p.args,
                &p.extra_env,
                &var_io,
                p.loop_source.is_some(),
            )
            .map_err(fail)?;
        task_env.extend(p.extra_env.iter().cloned());
        let dir = self.effective_dir(section);

        if let Some(cond) = &section.condition {
            let mut map: HashMap<String, String> = task_env.iter().cloned().collect();
            map.extend(self.env_overrides.iter().cloned());
            let ctx = CondContext { env: &map, dir };
            if !condition::evaluate(cond, &ctx).map_err(fail)? {
                observer.status(
                    name,
                    &TaskStatus::Skipped {
                        reason: format!("if: {}", cond),
                    },
                );
                return Ok(NodeOutcome::Skipped);
            }
        }

        let mut full_env = task_env.clone();
        full_env.extend(self.env_overrides.iter().cloned());

        if !self.dry_run {
            // Item variables of a `for.from` loop, and the vars that use them,
            // only exist once its items are resolved; `run_items` checks
            // those per item.
            let per_item: Vec<String> = if p.loop_source.is_some() {
                self.item_dependent(section)
            } else {
                Vec::new()
            };
            let process_env = self.prepare_env_vars(p.args, &task_env);
            let missing = self.missing_requires(section, &process_env, &per_item);
            if !missing.is_empty() {
                return Err(fail(Error::MissingVariable(
                    missing.join(", "),
                    section.title.clone(),
                )));
            }
        }

        if self.use_cache
            && !self.dry_run
            && !section.status.is_empty()
            && self.status_is_up_to_date(section, p.args, &task_env, dir)
        {
            observer.status(
                name,
                &TaskStatus::Skipped {
                    reason: "up to date (status)".into(),
                },
            );
            return Ok(NodeOutcome::Skipped);
        }
        let fingerprint = (self.use_cache && !self.dry_run && !section.sources.is_empty())
            .then(|| cache::fingerprint(section, &full_env, p.args, dir));
        if let Some(fp) = &fingerprint
            && cache::is_fresh_named(&self.cache_scope, name, section, fp, dir)
        {
            observer.status(
                name,
                &TaskStatus::Skipped {
                    reason: "up to date".into(),
                },
            );
            return Ok(NodeOutcome::Skipped);
        }

        let items: Option<Vec<String>> =
            p.loop_source.map(|source| source_items(&task_env, source));
        if let Some(items) = &items
            && items.is_empty()
        {
            observer.status(
                name,
                &TaskStatus::Skipped {
                    reason: "for: no items".into(),
                },
            );
            return Ok(NodeOutcome::Skipped);
        }

        if let Some(message) = &section.prompt {
            self.confirm(section, message, io).map_err(fail)?;
        }

        let started = Instant::now();
        let mut ignored = false;
        let result = match &items {
            Some(items) => self.run_items(p, &task_env, items, io),
            None => self.run_with_retries(p, &task_env, io),
        };
        let body = match result {
            // Only runtime failures are ignorable; cancellation and
            // configuration errors still stop the run.
            Err(e) if section.ignore_error && is_retriable(&e) => {
                io.observer
                    .output(name, Stream::Stderr, &format!("ignoring error: {}", e));
                ignored = true;
                Ok(String::new())
            }
            other => other,
        };
        // Deferred tasks run once the body is over, even when it failed.
        let deferred = if p.defers.is_empty() {
            Ok(())
        } else {
            self.run_deferred(p, io)
        };
        let output = match (body, deferred) {
            (Ok(out), Ok(())) => out,
            (Err(e), _) | (Ok(_), Err(e)) => return Err(fail(e)),
        };

        // A failure that was ignored must not be remembered as up to date.
        if let Some(fp) = &fingerprint
            && !ignored
        {
            cache::record(&self.cache_scope, name, fp);
        }

        observer.status(
            name,
            &TaskStatus::Succeeded {
                elapsed: started.elapsed(),
            },
        );

        let output = io
            .capture
            .then(|| output.trim_end_matches(['\n', '\r']).to_string());
        Ok(NodeOutcome::Done { output })
    }

    /// The `requires` names that are unset or empty in `env` (the variables
    /// the task process gets) and the process environment, ignoring `skip`.
    fn missing_requires(
        &self,
        section: &Section,
        env: &[(String, String)],
        skip: &[String],
    ) -> Vec<String> {
        section
            .requires
            .iter()
            .filter(|var| !skip.contains(var))
            .filter(|var| {
                let value = env
                    .iter()
                    .rev()
                    .find(|(k, _)| k == *var)
                    .map(|(_, v)| v.clone())
                    .or_else(|| std::env::var(var).ok());
                value.is_none_or(|v| v.is_empty())
            })
            .cloned()
            .collect()
    }

    /// True when every `status` command exits 0.
    fn status_is_up_to_date(
        &self,
        section: &Section,
        args: &[String],
        task_env: &[(String, String)],
        dir: Option<&Path>,
    ) -> bool {
        section.status.iter().all(|cmd| {
            #[cfg(unix)]
            let mut command = {
                let mut c = Command::new("sh");
                c.arg("-c").arg(cmd);
                c
            };
            #[cfg(not(unix))]
            let mut command = {
                let mut c = Command::new("cmd");
                c.arg("/C").arg(cmd);
                c
            };
            command
                .envs(self.prepare_env_vars(args, task_env))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(dir) = dir {
                command.current_dir(dir);
            }
            command.status().map(|s| s.success()).unwrap_or(false)
        })
    }

    /// Ask to confirm a task's `prompt`. Piped runs (parallel, TUI) cannot ask.
    fn confirm(&self, section: &Section, message: &str, io: &TaskIo) -> Result<()> {
        if self.dry_run {
            println!("[dry-run] prompt: {}", message);
            return Ok(());
        }
        if self.assume_yes {
            return Ok(());
        }
        let mut confirmed = self.confirmed.lock().unwrap_or_else(|e| e.into_inner());
        if confirmed.contains(&section.title) {
            return Ok(());
        }
        if io.piped {
            return Err(Error::ConfirmationRequired(section.title.clone()));
        }

        eprint!("? {} [y/N] ", message);
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).map_err(Error::Io)?;
        if matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            confirmed.insert(section.title.clone());
            Ok(())
        } else {
            Err(Error::Declined(section.title.clone()))
        }
    }

    /// Run the task once per item, stopping at the first failure.
    fn run_items(
        &self,
        p: &NodeParams,
        task_env: &[(String, String)],
        items: &[String],
        io: &TaskIo,
    ) -> Result<String> {
        let mut captured = String::new();
        for (index, item) in items.iter().enumerate() {
            if io.cancel.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            io.observer
                .output(p.label, Stream::Stdout, &format!("↳ {}[{}]", p.label, item));
            let mut env = task_env.to_vec();
            let item_vars = loop_item_vars(p.section, index, item);
            env.extend(item_vars.iter().cloned());
            let var_io = TaskIo {
                deadline: p.section.timeout.map(|t| Instant::now() + t),
                ..*io
            };
            env.extend(self.item_env(p.section, task_env, &item_vars, &var_io)?);
            if !self.dry_run {
                let missing =
                    self.missing_requires(p.section, &self.prepare_env_vars(p.args, &env), &[]);
                if !missing.is_empty() {
                    return Err(Error::MissingVariable(
                        missing.join(", "),
                        p.section.title.clone(),
                    ));
                }
            }
            captured.push_str(&self.run_with_retries(p, &env, io)?);
        }
        Ok(captured)
    }

    /// Run a task's code blocks, retrying runtime failures per `retries`.
    fn run_with_retries(
        &self,
        p: &NodeParams,
        task_env: &[(String, String)],
        io: &TaskIo,
    ) -> Result<String> {
        let section = p.section;
        let max_attempts = section.retries.saturating_add(1);
        let mut attempt = 1;
        loop {
            let attempt_io = TaskIo {
                deadline: section.timeout.map(|t| Instant::now() + t),
                ..*io
            };
            match self.run_blocks(section, p.args, p.lang_filter, task_env, &attempt_io) {
                Ok(out) => return Ok(out),
                Err(e) if attempt < max_attempts && is_retriable(&e) => {
                    let delay = section.retry_delay.unwrap_or(Duration::from_secs(1));
                    io.observer.status(
                        p.label,
                        &TaskStatus::Retrying {
                            attempt,
                            max: max_attempts,
                            delay,
                        },
                    );
                    if !sleep_unless_cancelled(delay, io.cancel) {
                        return Err(Error::Cancelled);
                    }
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Run the `defer` tasks in order. They ignore cancellation and timeouts
    /// so cleanup always happens; all run, and the first error is returned.
    fn run_deferred(&self, p: &NodeParams, io: &TaskIo) -> Result<()> {
        let never = AtomicBool::new(false);
        let io = TaskIo {
            cancel: &never,
            deadline: None,
            capture: false,
            ..*io
        };
        let mut first: Option<Error> = None;
        for d in p.defers {
            let result = self
                .task_env(d.section, &d.args, &p.extra_env, &io, false)
                .and_then(|mut env| {
                    env.extend(p.extra_env.iter().cloned());
                    self.run_blocks(d.section, &d.args, None, &env, &io)
                });
            if let Err(e) = result {
                io.observer.output(
                    p.label,
                    Stream::Stderr,
                    &format!("defer '{}' failed: {}", d.section.title, e),
                );
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Execute a single code block, outside any task lifecycle.
    pub fn execute_code(
        &self,
        lang: &str,
        code: &str,
        args: &[String],
        param_env: &[(String, String)],
        working_dir: Option<&Path>,
    ) -> Result<()> {
        let cancel = AtomicBool::new(false);
        let io = TaskIo {
            name: lang,
            observer: &NullObserver,
            piped: false,
            capture: false,
            cancel: &cancel,
            deadline: None,
        };
        self.execute_code_with_params(lang, code, args, param_env, working_dir, &io)?;
        Ok(())
    }

    fn execute_code_with_params(
        &self,
        lang: &str,
        code: &str,
        args: &[String],
        param_env: &[(String, String)],
        working_dir: Option<&Path>,
        io: &TaskIo,
    ) -> Result<String> {
        let runtime = self
            .config
            .get_runtime(lang)
            .ok_or_else(|| Error::RuntimeNotFound(lang.to_string()))?;

        let parts: Vec<&str> = runtime.split_whitespace().collect();
        if parts.is_empty() {
            return Err(Error::RuntimeNotFound(lang.to_string()));
        }

        if self.verbose && !self.dry_run {
            show_block(lang, code, working_dir, io);
        }

        if self.dry_run {
            let fmt_pairs = |pairs: &[(String, String)]| {
                pairs
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let mut text = format!(
                "[dry-run] lang: {}\n[dry-run] runtime: {}\n[dry-run] code:\n{}",
                lang, runtime, code
            );
            if !args.is_empty() {
                text.push_str(&format!("\n[dry-run] args: {}", args.join(" ")));
            }
            if !param_env.is_empty() {
                text.push_str(&format!("\n[dry-run] params: {}", fmt_pairs(param_env)));
            }
            if !self.env_overrides.is_empty() {
                text.push_str(&format!(
                    "\n[dry-run] env: {}",
                    fmt_pairs(&self.env_overrides)
                ));
            }
            if let Some(dir) = working_dir {
                text.push_str(&format!("\n[dry-run] dir: {}", dir.display()));
            }
            println!("{}", text);
            return Ok(String::new());
        }

        let mut command = Command::new(parts[0]);
        command.args(&parts[1..]);
        command.envs(self.prepare_env_vars(args, param_env));
        if let Some(dir) = working_dir {
            command.current_dir(dir);
        }

        match self.config.get_execution_mode(lang) {
            ExecutionMode::Stdin => self.run_command(command, Some(code), None, io),
            ExecutionMode::Arg => {
                command.arg(code);
                self.run_command(command, None, None, io)
            }
            ExecutionMode::File => {
                let temp_file = write_temp_file(lang, code)?;
                command.arg(&temp_file);
                let result = self.run_command(command, None, Some(lang), io);
                fs::remove_file(&temp_file).ok();
                result
            }
        }
    }

    /// Spawn `command` and wait for it, honoring cancellation and the
    /// deadline. Returns captured stdout when `io.capture` is set.
    fn run_command(
        &self,
        mut command: Command,
        stdin_code: Option<&str>,
        file_lang: Option<&str>,
        io: &TaskIo,
    ) -> Result<String> {
        let pipe_out = io.piped || io.capture;
        command
            .stdout(if pipe_out {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .stderr(if io.piped {
                Stdio::piped()
            } else {
                Stdio::inherit()
            });
        if stdin_code.is_some() {
            command.stdin(Stdio::piped());
        } else if io.piped {
            command.stdin(Stdio::null());
        }

        // Give the task its own process group when it is detached from the
        // terminal (parallel/TUI) or has a timeout, so that killing it also
        // takes down grandchildren (`bash -c 'sleep 60'`) that would
        // otherwise keep our output pipes open. Plain sequential tasks stay
        // in the terminal's group so Ctrl+C and tty input work as usual.
        #[cfg(unix)]
        let own_group = io.piped || io.deadline.is_some();
        #[cfg(unix)]
        if own_group {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command.spawn().map_err(|e| match file_lang {
            Some(lang) => Error::Execution(format!("Failed to execute {}: {}", lang, e)),
            None => Error::Execution(format!("Failed to spawn process: {}", e)),
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();

        #[cfg(unix)]
        let own_group_value = own_group;
        #[cfg(not(unix))]
        let own_group_value = false;

        let (status, captured) = std::thread::scope(|s| {
            let out_handle =
                stdout.map(|r| s.spawn(move || pump(r, Stream::Stdout, io, io.capture)));
            let err_handle = stderr.map(|r| s.spawn(move || pump(r, Stream::Stderr, io, false)));

            let mut write_error: Option<Error> = None;
            let own_group_flag = own_group_value;

            if let (Some(mut stdin), Some(code)) = (stdin, stdin_code) {
                match stdin.write_all(code.as_bytes()) {
                    Ok(()) => {}
                    // The child exited before reading everything; its exit
                    // status tells the real story.
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                    Err(e) => {
                        write_error =
                            Some(Error::Execution(format!("Failed to write to stdin: {}", e)));
                        kill_task(&mut child, own_group_flag);
                    }
                }
                drop(stdin);
            }

            let result = match write_error {
                None => wait_for(&mut child, io, own_group_flag),
                Some(e) => {
                    let _ = child.wait();
                    Err(e)
                }
            };

            let captured = out_handle
                .map(|h| h.join().unwrap_or_default())
                .unwrap_or_default();
            if let Some(h) = err_handle {
                let _ = h.join();
            }
            (result, captured)
        });

        let status = status?;
        if !status.success() {
            return Err(Error::ExecutionFailed(exit_code_from_status(&status)));
        }
        Ok(captured)
    }
}

/// Terminate a task. With its own process group the whole group is signalled
/// (politely first), otherwise only the direct child is killed.
fn kill_task(child: &mut std::process::Child, own_group: bool) {
    #[cfg(unix)]
    if own_group {
        let pgid = child.id() as libc::pid_t;
        // SAFETY: plain signal delivery to a process group we created.
        unsafe { libc::killpg(pgid, libc::SIGTERM) };
        let grace = Instant::now() + Duration::from_millis(300);
        while Instant::now() < grace {
            if matches!(child.try_wait(), Ok(Some(_))) {
                // The leader exited; sweep any stragglers in the group.
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
        let _ = child.wait();
        return;
    }
    let _ = own_group;
    let _ = child.kill();
    let _ = child.wait();
}

/// Wait for `child`, killing it on cancellation or deadline expiry.
pub(crate) fn wait_for(
    child: &mut std::process::Child,
    io: &TaskIo,
    own_group: bool,
) -> Result<ExitStatus> {
    let mut nap = Duration::from_millis(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) => {
                return Err(Error::Execution(format!(
                    "Failed to wait for process: {}",
                    e
                )));
            }
        }
        // A task in its own process group no longer receives the terminal's
        // Ctrl+C, so forward it.
        let interrupted = own_group && crate::INTERRUPTED.load(Ordering::SeqCst);
        if io.cancel.load(Ordering::SeqCst) || interrupted {
            kill_task(child, own_group);
            return Err(Error::Cancelled);
        }
        if let Some(deadline) = io.deadline
            && Instant::now() >= deadline
        {
            kill_task(child, own_group);
            return Err(Error::Timeout(io.name.to_string()));
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(20));
    }
}

/// Forward each line of `reader` to the observer; optionally keep a copy.
fn pump<R: Read>(reader: R, stream: Stream, io: &TaskIo, keep: bool) -> String {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    let mut kept = String::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buf);
                let line = text.trim_end_matches(['\n', '\r']);
                io.observer.output(io.name, stream, line);
                if keep {
                    kept.push_str(line);
                    kept.push('\n');
                }
            }
        }
    }
    kept
}

/// Sleep for `delay`, returning false early if `cancel` was raised.
fn sleep_unless_cancelled(delay: Duration, cancel: &AtomicBool) -> bool {
    let end = Instant::now() + delay;
    while Instant::now() < end {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20).min(end - Instant::now()));
    }
    !cancel.load(Ordering::SeqCst)
}

/// Whether `name` could be a parameter name (`region`, `build-type`).
fn is_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Only runtime failures are worth retrying; configuration mistakes
/// (missing runtime, bad params, ...) would fail identically every time.
fn is_retriable(e: &Error) -> bool {
    matches!(
        e,
        Error::ExecutionFailed(_) | Error::Timeout(_) | Error::Execution(_)
    )
}

fn write_temp_file(lang: &str, code: &str) -> Result<PathBuf> {
    let file_ext = match lang {
        "go" | "golang" => "go",
        "python" => "py",
        "ruby" => "rb",
        "javascript" | "js" => "js",
        "typescript" | "ts" => "ts",
        _ => lang,
    };

    // Nanosecond timestamps alone can collide under concurrent execution
    // (clock resolution varies by platform), so mix in the process id and a
    // per-process counter.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = format!(
        "mx_temp_{}_{}_{}.{}",
        timestamp,
        std::process::id(),
        counter,
        file_ext
    );
    let temp_file = std::env::temp_dir().join(file_name);
    fs::write(&temp_file, code)
        .map_err(|e| Error::Execution(format!("Failed to write temp file: {}", e)))?;
    Ok(temp_file)
}
