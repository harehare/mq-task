use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mq_lang::{Engine, Ident, RuntimeValue, parse_markdown_input};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ExecutionMode};
use crate::error::{Error, Result};

const SECTIONS_QUERY: &str = include_str!("../sections.mq");

/// `(env, dir)` defaults parsed from a document-wide `meta` block.
type GlobalDefaults = (Vec<(String, String)>, Option<PathBuf>);

/// Represents a code block in a section
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeBlock {
    /// Language of the code block
    pub lang: String,
    /// Code content
    pub code: String,
}

/// Document-wide defaults parsed from a `meta` code block that appears
/// before the first heading in the file. Applies to every task; a task's
/// own `meta` block overrides same-named keys, and CLI flags override both.
#[derive(Debug, Deserialize, Default)]
struct GlobalMeta {
    /// Default environment variables applied to every task
    #[serde(default)]
    env: Vec<String>,
    /// Default working directory applied to every task without its own `dir`
    #[serde(default)]
    dir: Option<String>,
}

/// Task metadata parsed from a `meta` code block within a section
#[derive(Debug, Deserialize, Default)]
struct TaskMeta {
    #[serde(default)]
    depends: Vec<String>,
    #[serde(default)]
    params: Vec<String>,
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
}

/// A named task parameter, e.g. `params = ["env=staging", "verbose"]`.
/// A bare name is required; `name=value` supplies a default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ParamDef {
    pub name: String,
    pub default: Option<String>,
}

impl ParamDef {
    fn parse(raw: &str) -> Self {
        match raw.split_once('=') {
            Some((name, default)) => ParamDef {
                name: name.trim().to_string(),
                default: Some(default.trim().to_string()),
            },
            None => ParamDef {
                name: raw.trim().to_string(),
                default: None,
            },
        }
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
}

/// Task runner that executes code blocks in Markdown sections
pub struct Runner {
    config: Config,
    engine: Engine,
    dry_run: bool,
    env_overrides: Vec<(String, String)>,
    working_dir: Option<std::path::PathBuf>,
    /// Document-wide `env`/`dir` defaults declared in a preamble `meta`
    /// block, parsed by the most recent call to `extract_sections`.
    global_env: Vec<(String, String)>,
    global_dir: Option<PathBuf>,
}

impl Runner {
    /// Create a new Runner with the given configuration
    pub fn new(config: Config) -> Self {
        let mut engine: Engine = Engine::default();
        engine.load_builtin_module();

        Self {
            config,
            engine,
            dry_run: false,
            env_overrides: Vec::new(),
            working_dir: None,
            global_env: Vec::new(),
            global_dir: None,
        }
    }

    /// Create a new Runner with default configuration
    pub fn with_default_config() -> Self {
        Self::new(Config::default())
    }

    /// Enable or disable dry-run mode
    pub fn set_dry_run(&mut self, dry_run: bool) {
        self.dry_run = dry_run;
    }

    /// Set environment variables injected into every task process for this run,
    /// e.g. from repeated `--env KEY=VALUE` CLI flags. Unlike declared `params`,
    /// these need no `meta` declaration in the task file.
    pub fn set_env_overrides(&mut self, env_overrides: Vec<(String, String)>) {
        self.env_overrides = env_overrides;
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
        self.working_dir = working_dir;
    }

    /// Load and parse a Markdown file
    pub fn load_markdown<P: AsRef<Path>>(&self, path: P) -> Result<String> {
        fs::read_to_string(path).map_err(Error::Io)
    }

    /// Extract sections from Markdown content
    pub fn extract_sections(&mut self, markdown: &str) -> Result<Vec<Section>> {
        let input = parse_markdown_input(markdown)
            .map_err(|e| Error::Markdown(format!("Failed to parse markdown: {}", e)))?;

        let (global_env, global_dir) = self.parse_global_meta(input.clone())?;
        self.global_env = global_env;
        self.global_dir = global_dir;

        let query = format!("{}\n | nodes | sections_with_code()", SECTIONS_QUERY);
        let result = self
            .engine
            .eval(&query, input.into_iter())
            .map_err(|e| Error::Query(format!("Failed to execute query: {}", e)))?;

        let sections = self.parse_sections(result)?;

        Ok(sections)
    }

    /// Parse the document-wide `meta` block declared before the first
    /// heading, if any, into `env`/`dir` defaults for every task.
    fn parse_global_meta(&mut self, input: Vec<RuntimeValue>) -> Result<GlobalDefaults> {
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

        let meta = codes
            .iter()
            .find(|c| c.lang == "meta")
            .and_then(|c| toml::from_str::<GlobalMeta>(&c.code).ok())
            .unwrap_or_default();

        let env = Self::parse_env_overrides(&meta.env)?;
        let dir = meta.dir.map(PathBuf::from);

        Ok((env, dir))
    }

    fn parse_sections(&self, result: mq_lang::RuntimeValues) -> Result<Vec<Section>> {
        let mut sections = Vec::new();

        for value in result.into_iter() {
            if let RuntimeValue::Dict(dict) = value {
                let section = self.parse_section(&dict)?;
                sections.push(section);
            }
        }

        Ok(sections)
    }

    fn parse_section(&self, dict: &BTreeMap<Ident, RuntimeValue>) -> Result<Section> {
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

        let meta = all_codes
            .iter()
            .find(|c| c.lang == "meta")
            .and_then(|c| toml::from_str::<TaskMeta>(&c.code).ok())
            .unwrap_or_default();

        let params = meta.params.iter().map(|p| ParamDef::parse(p)).collect();
        let private = meta.private || title.starts_with('_');
        let env = Self::parse_env_overrides(&meta.env)?;
        let dir = meta.dir.map(PathBuf::from);

        let codes: Vec<CodeBlock> = all_codes.into_iter().filter(|c| c.lang != "meta").collect();

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

                blocks.push(CodeBlock { lang, code });
            }
        }

        Ok(blocks)
    }

    pub fn find_section<'a>(&self, sections: &'a [Section], title: &str) -> Option<&'a Section> {
        sections
            .iter()
            .find(|s| s.title == title || s.aliases.iter().any(|a| a == title))
    }

    fn resolve_execution_order<'a>(
        &self,
        sections: &'a [Section],
        target: &str,
    ) -> Result<Vec<&'a Section>> {
        let mut visited = HashSet::new();
        let mut in_progress = HashSet::new();
        let mut order = Vec::new();
        self.dfs_resolve(sections, target, &mut visited, &mut in_progress, &mut order)?;
        Ok(order)
    }

    fn dfs_resolve<'a>(
        &self,
        sections: &'a [Section],
        task_name: &str,
        visited: &mut HashSet<String>,
        in_progress: &mut HashSet<String>,
        order: &mut Vec<&'a Section>,
    ) -> Result<()> {
        if in_progress.contains(task_name) {
            return Err(Error::CircularDependency(task_name.to_string()));
        }
        if visited.contains(task_name) {
            return Ok(());
        }

        in_progress.insert(task_name.to_string());

        let section = self
            .find_section(sections, task_name)
            .ok_or_else(|| Error::SectionNotFound(task_name.to_string()))?;

        let deps = section.depends.clone();
        for dep in &deps {
            self.dfs_resolve(sections, dep, visited, in_progress, order)?;
        }

        in_progress.remove(task_name);
        visited.insert(task_name.to_string());
        order.push(section);

        Ok(())
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
        // Document-wide `meta` env applies first, then the task's own `meta`
        // env (which overrides same-named keys), then bound params; CLI
        // `--env` overrides (applied in prepare_env_vars) take precedence over all.
        let mut task_env = self.global_env.clone();
        task_env.extend(section.env.clone());
        task_env.extend(Self::bind_params(&section.params, args, &section.title)?);

        // CLI `--dir` overrides a `meta`-declared working directory for this
        // task, which in turn overrides the document-wide `meta` default.
        let working_dir = self
            .working_dir
            .as_deref()
            .or(section.dir.as_deref())
            .or(self.global_dir.as_deref());

        for code_block in &section.codes {
            if code_block.lang.is_empty() {
                continue;
            }

            // Apply language filter if specified
            if let Some(filter) = lang_filter
                && code_block.lang != filter
            {
                continue;
            }

            self.execute_code_with_params(
                &code_block.lang,
                &code_block.code,
                args,
                &task_env,
                working_dir,
            )?;
        }

        Ok(())
    }

    /// Bind declared params to CLI args: `name=value` binds by name, the
    /// rest fill remaining params positionally, then defaults. Returns
    /// `MX_PARAM_<NAME>` env pairs; errors if a required param is unbound.
    fn bind_params(
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

    pub fn execute_code(&self, lang: &str, code: &str) -> Result<()> {
        self.execute_code_with_args(lang, code, &[])
    }

    pub fn execute_code_with_args(&self, lang: &str, code: &str, args: &[String]) -> Result<()> {
        self.execute_code_with_params(lang, code, args, &[], self.working_dir.as_deref())
    }

    fn execute_code_with_params(
        &self,
        lang: &str,
        code: &str,
        args: &[String],
        param_env: &[(String, String)],
        working_dir: Option<&Path>,
    ) -> Result<()> {
        let runtime = self
            .config
            .get_runtime(lang)
            .ok_or_else(|| Error::RuntimeNotFound(lang.to_string()))?;

        let parts: Vec<&str> = runtime.split_whitespace().collect();
        if parts.is_empty() {
            return Err(Error::RuntimeNotFound(lang.to_string()));
        }

        // Get execution mode from config
        let execution_mode = self.config.get_execution_mode(lang);

        if self.dry_run {
            let args_line = if !args.is_empty() {
                format!("\n[dry-run] args: {}", args.join(" "))
            } else {
                String::new()
            };
            let params_line = if !param_env.is_empty() {
                format!(
                    "\n[dry-run] params: {}",
                    param_env
                        .iter()
                        .map(|(k, v)| format!("{}={}", k, v))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            } else {
                String::new()
            };
            let env_line = if !self.env_overrides.is_empty() {
                format!(
                    "\n[dry-run] env: {}",
                    self.env_overrides
                        .iter()
                        .map(|(k, v)| format!("{}={}", k, v))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            } else {
                String::new()
            };
            let dir_line = if let Some(dir) = working_dir {
                format!("\n[dry-run] dir: {}", dir.display())
            } else {
                String::new()
            };
            println!(
                "[dry-run] lang: {}\n[dry-run] runtime: {}\n[dry-run] code:\n{}{}{}{}{}",
                lang, runtime, code, args_line, params_line, env_line, dir_line
            );
            return Ok(());
        }

        match execution_mode {
            ExecutionMode::File => self.execute_code_with_file_and_args(
                lang,
                code,
                &parts,
                args,
                param_env,
                working_dir,
            ),
            ExecutionMode::Arg => {
                self.execute_code_with_arg_mode(code, &parts, args, param_env, working_dir)
            }
            ExecutionMode::Stdin => {
                self.execute_code_with_stdin_and_args(code, &parts, args, param_env, working_dir)
            }
        }
    }

    fn execute_code_with_stdin_and_args(
        &self,
        code: &str,
        parts: &[&str],
        task_args: &[String],
        param_env: &[(String, String)],
        working_dir: Option<&Path>,
    ) -> Result<()> {
        let cmd = parts[0];
        let args = &parts[1..];

        // Use inherit() for stdout/stderr to preserve TTY and colors
        let mut command = Command::new(cmd);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .envs(self.prepare_env_vars(task_args, param_env));
        if let Some(dir) = working_dir {
            command.current_dir(dir);
        }
        let mut child = command
            .spawn()
            .map_err(|e| Error::Execution(format!("Failed to spawn process: {}", e)))?;

        // Write code to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(code.as_bytes())
                .map_err(|e| Error::Execution(format!("Failed to write to stdin: {}", e)))?;
            drop(stdin);
        }

        // Wait for completion
        let status = child
            .wait()
            .map_err(|e| Error::Execution(format!("Failed to wait for process: {}", e)))?;

        if !status.success() {
            return Err(Error::ExecutionFailed(status.code().unwrap_or(1)));
        }

        Ok(())
    }

    fn execute_code_with_arg_mode(
        &self,
        code: &str,
        parts: &[&str],
        task_args: &[String],
        param_env: &[(String, String)],
        working_dir: Option<&Path>,
    ) -> Result<()> {
        let cmd = parts[0];
        // Append code as an argument to the command
        let mut args: Vec<&str> = parts[1..].to_vec();
        args.push(code);

        // Use inherit() for stdin/stdout/stderr to preserve TTY, colors, and interactivity
        let mut command = Command::new(cmd);
        command
            .args(args)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .envs(self.prepare_env_vars(task_args, param_env));
        if let Some(dir) = working_dir {
            command.current_dir(dir);
        }
        let status = command
            .status()
            .map_err(|e| Error::Execution(format!("Failed to spawn process: {}", e)))?;

        if !status.success() {
            return Err(Error::ExecutionFailed(status.code().unwrap_or(1)));
        }

        Ok(())
    }

    fn execute_code_with_file_and_args(
        &self,
        lang: &str,
        code: &str,
        parts: &[&str],
        task_args: &[String],
        param_env: &[(String, String)],
        working_dir: Option<&Path>,
    ) -> Result<()> {
        use std::env;

        // Create temporary directory
        let temp_dir = env::temp_dir();

        // Use language name as file extension, or map known languages
        let file_ext = match lang {
            "go" | "golang" => "go",
            "python" => "py",
            "ruby" => "rb",
            "javascript" | "js" => "js",
            "typescript" | "ts" => "ts",
            _ => lang, // Use language name as extension for custom languages
        };

        // Generate a unique file name. Nanosecond timestamps alone can collide
        // under concurrent execution (clock resolution varies by platform), so
        // mix in the process id and a per-process counter.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file_name = format!(
            "mx_temp_{}_{}_{}.{}",
            timestamp,
            std::process::id(),
            counter,
            file_ext
        );
        let temp_file = temp_dir.join(&file_name);

        // Write code to temporary file
        fs::write(&temp_file, code)
            .map_err(|e| Error::Execution(format!("Failed to write temp file: {}", e)))?;

        // Execute go run <file>
        let mut command = Command::new(parts[0]);
        command
            .args(&parts[1..])
            .arg(&temp_file)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .envs(self.prepare_env_vars(task_args, param_env));
        if let Some(dir) = working_dir {
            command.current_dir(dir);
        }
        let status = command
            .status()
            .map_err(|e| Error::Execution(format!("Failed to execute {}: {}", lang, e)))?;

        // Clean up temporary file
        fs::remove_file(&temp_file).ok();

        if !status.success() {
            Err(Error::ExecutionFailed(status.code().unwrap_or(1)))
        } else {
            Ok(())
        }
    }

    /// Prepare environment variables from task arguments, bound named parameters,
    /// and `--env` overrides. Overrides are applied last so they take precedence
    /// over any same-named `MX_*` variable.
    fn prepare_env_vars(
        &self,
        args: &[String],
        param_env: &[(String, String)],
    ) -> Vec<(String, String)> {
        let mut env_vars = Vec::new();

        // Set MX_ARGS with all arguments joined by space
        if !args.is_empty() {
            env_vars.push(("MX_ARGS".to_string(), args.join(" ")));
        }

        env_vars.extend(param_env.iter().cloned());

        // Set individual arguments as MX_ARG_0, MX_ARG_1, etc.
        for (i, arg) in args.iter().enumerate() {
            env_vars.push((format!("MX_ARG_{}", i), arg.clone()));
        }

        env_vars.extend(self.env_overrides.iter().cloned());

        env_vars
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
        let markdown = self.load_markdown(markdown_path)?;
        let sections = self.extract_sections(&markdown)?;

        let execution_order = self.resolve_execution_order(&sections, task_name)?;
        // Resolve alias to canonical title for the is_dep check below.
        let primary_title = self
            .find_section(&sections, task_name)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| task_name.to_string());

        for section in execution_order {
            let is_dep = section.title != primary_title;
            if is_dep {
                println!("Running dependency: {}\n", section.title);
            }
            self.execute_section_with_lang_filter(
                section,
                if is_dep { &[] } else { args },
                lang_filter,
            )?;
        }

        Ok(())
    }

    /// List all available tasks (sections) in a Markdown file
    pub fn list_tasks<P: AsRef<Path>>(&mut self, markdown_path: P) -> Result<Vec<String>> {
        let markdown = self.load_markdown(markdown_path)?;
        let sections = self.extract_sections(&markdown)?;

        Ok(sections
            .into_iter()
            .map(|s| format!("{}: {}", s.title, s.description.unwrap_or_default()))
            .collect())
    }

    /// List all available task sections in a Markdown file with their details
    pub fn list_task_sections<P: AsRef<Path>>(&mut self, markdown_path: P) -> Result<Vec<Section>> {
        let markdown = self.load_markdown(markdown_path)?;
        self.extract_sections(&markdown)
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
        let resolved = runner.working_dir.as_deref().or(section.dir.as_deref());
        assert_eq!(resolved, Some(Path::new("services/api")));

        // CLI --dir set: it wins over the meta default.
        let mut runner = Runner::with_default_config();
        runner.set_working_dir(Some(PathBuf::from("/tmp/override")));
        let resolved = runner.working_dir.as_deref().or(section.dir.as_deref());
        assert_eq!(resolved, Some(Path::new("/tmp/override")));
    }

    #[test]
    fn test_bind_params_uses_named_positional_and_default() {
        let params = vec![
            ParamDef {
                name: "env".to_string(),
                default: Some("staging".to_string()),
            },
            ParamDef {
                name: "region".to_string(),
                default: None,
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
            runner.global_env,
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

        assert_eq!(runner.global_dir, Some(PathBuf::from("services/api")));
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

        assert!(runner.global_env.is_empty());
        assert!(runner.global_dir.is_none());
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
        let mut task_env = runner.global_env.clone();
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
        runner.global_dir = Some(PathBuf::from("services/default"));

        // Task's own dir wins over the global default.
        let resolved = runner
            .working_dir
            .as_deref()
            .or(section.dir.as_deref())
            .or(runner.global_dir.as_deref());
        assert_eq!(resolved, Some(Path::new("services/api")));

        // Falls back to the global default when the task declares none.
        let section_no_dir = Section {
            title: "build".to_string(),
            codes: vec![],
            ..Default::default()
        };
        let resolved = runner
            .working_dir
            .as_deref()
            .or(section_no_dir.dir.as_deref())
            .or(runner.global_dir.as_deref());
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
}
