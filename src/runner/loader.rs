//! Reading a task file: loading, import resolution and turning the parsed
//! Markdown into [`Section`]s.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mq_lang::{DictMap, Ident, RuntimeValue, parse_markdown_input};

use super::{Collected, Runner, SECTIONS_QUERY};
use crate::config::RuntimeConfig;
use crate::error::{Error, Result};
use crate::schedule::split_words;
use crate::section::{
    CodeBlock, GlobalMeta, Globals, ImportSpec, LoopSpec, ParamDef, Section, TaskMeta,
};
use crate::util::parse_duration;
use crate::vars;

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

impl Runner {
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
        self.file_default_task = globals.default_task;
        for (lang, runtime) in globals.runtimes {
            if !self.pinned_runtimes.contains(&lang) && self.document_runtimes.insert(lang.clone())
            {
                self.exec.config.runtimes.insert(lang, runtime);
            }
        }

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
        self.document_runtimes.clear();
        let mut stack = Vec::new();
        let sections = match self.load_file(path.as_ref(), &mut stack) {
            Ok((sections, main)) => {
                self.default_task = main.default_task;
                sections
            }
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
        self.document_runtimes.clear();
        let (sections, main) = self.load_file(path.as_ref(), &mut stack)?;
        self.default_task = main.default_task;
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
            runtimes: Vec::new(),
            default_task: self.file_default_task.take(),
        };

        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let current = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut namespaces = HashSet::new();
        for spec in &globals.imports {
            let import = spec.path.as_str();
            let targets = match spec.resolve(base, &current) {
                Ok(targets) => targets,
                Err(e) => match self.collected.as_mut() {
                    Some(collected) => {
                        collected.errors.push(e);
                        continue;
                    }
                    None => return Err(e),
                },
            };
            for (import_path, stem) in targets {
                if !spec.flatten && !namespaces.insert(stem.clone()) {
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
                if !spec.flatten {
                    namespace_sections(&mut imported, &stem);
                }
                sections.extend(imported);
            }
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
            imports: meta
                .imports
                .iter()
                .map(ImportSpec::from_toml)
                .collect::<Result<_>>()?,
            dotenv: meta.dotenv,
            vars: match &meta.vars {
                Some(table) => vars::parse_table("document `meta` block", table)?,
                None => Vec::new(),
            },
            runtimes: match meta.runtimes {
                Some(table) => table
                    .into_iter()
                    .map(|(lang, value)| {
                        value
                            .try_into::<RuntimeConfig>()
                            .map(|runtime| (lang.clone(), runtime))
                            .map_err(|e| {
                                Error::Invalid(format!(
                                    "document `meta` block: `runtimes`: '{}' must be a command or {{ command, execution_mode }}: {}",
                                    lang,
                                    e.message()
                                ))
                            })
                    })
                    .collect::<Result<_>>()?,
                None => Vec::new(),
            },
            default_task: meta.default_task,
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
}

/// Fold an imported file's document-wide defaults into its tasks so they
/// keep behaving as they would when run from that file directly.
pub(super) fn apply_globals(sections: &mut [Section], globals: &Globals) {
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
pub(super) fn namespace_sections(sections: &mut [Section], namespace: &str) {
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
pub(super) fn unknown_field(message: &str) -> Option<(String, Vec<String>)> {
    let rest = message.strip_prefix("unknown field ")?;
    let mut quoted = rest.split('`').skip(1).step_by(2).map(str::to_string);
    let key = quoted.next()?;
    Some((key, quoted.collect()))
}

/// Turn a `meta` block parse error into one a user can act on: unknown keys
/// get a "did you mean" hint and the line they are on.
pub(super) fn meta_error(
    context: &str,
    code: &str,
    err: &toml::de::Error,
    document: bool,
) -> Error {
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
pub(super) fn quote_word(word: &str) -> String {
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
