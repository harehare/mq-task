//! `mq-task check`: validate a task file without running anything.
//!
//! Everything wrong is reported at once: broken `meta` blocks, unknown
//! dependencies, cycles, missing runtimes, bad globs and conditions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use crate::condition::{self, CondContext};
use crate::config::Config;
use crate::error::Error;
use crate::runner::{LoopSpec, Runner, Section};
use crate::schedule::{build_plan, resolve_call};
use crate::util::closest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// One problem found in a task file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    /// The task it concerns, when there is one.
    pub task: Option<String>,
    pub message: String,
}

impl Diagnostic {
    fn error(task: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            task: task.map(str::to_string),
            message: message.into(),
        }
    }

    fn warning(task: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(task, message)
        }
    }
}

/// The outcome of checking a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Number of tasks that were checked.
    pub tasks: usize,
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    pub fn errors(&self) -> usize {
        self.count(Severity::Error)
    }

    pub fn warnings(&self) -> usize {
        self.count(Severity::Warning)
    }

    fn count(&self, severity: Severity) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == severity)
            .count()
    }

    /// True when the file passes; warnings fail it only if `strict`.
    pub fn passes(&self, strict: bool) -> bool {
        self.errors() == 0 && (!strict || self.warnings() == 0)
    }
}

/// Check the task file at `path` (with its imports) against `config`.
/// `reserved` are names of commands that a task name would collide with.
pub fn check_file(
    runner: &mut Runner,
    config: &Config,
    path: &Path,
    reserved: &[String],
) -> Report {
    let (sections, errors, failed) = runner.load_sections_lenient(path);
    let mut diagnostics: Vec<Diagnostic> = errors
        .iter()
        .map(|e| Diagnostic::error(None, plain_message(e)))
        .collect();
    diagnostics.extend(check_sections(&sections, &failed, config, reserved));
    // Errors first, each group in the order found.
    diagnostics.sort_by_key(|d| d.severity != Severity::Error);
    Report {
        tasks: sections
            .iter()
            .filter(|s| !s.codes.is_empty() || !s.depends.is_empty())
            .count(),
        diagnostics,
    }
}

/// Everything that can be checked once the tasks are loaded. `failed` lists
/// tasks that could not be parsed; references to them are not reported again.
pub fn check_sections(
    sections: &[Section],
    failed: &[String],
    config: &Config,
    reserved: &[String],
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_names(sections, reserved, &mut out);
    check_references(sections, failed, &mut out);
    check_cycles(sections, &mut out);
    check_plans(sections, &mut out);
    check_runtimes(sections, config, &mut out);
    check_patterns(sections, &mut out);
    check_conditions(sections, &mut out);
    out
}

/// The error text without the category prefix ("Invalid task file: ...").
fn plain_message(e: &Error) -> String {
    match e {
        Error::Invalid(m) | Error::Config(m) => m.clone(),
        other => other.to_string(),
    }
}

fn runnable(section: &Section) -> bool {
    !section.codes.is_empty() || !section.depends.is_empty()
}

/// Duplicate titles and aliases, repeated parameters, and names that the
/// command line would take for a command.
fn check_names(sections: &[Section], reserved: &[String], out: &mut Vec<Diagnostic>) {
    let mut owner: HashMap<&str, &str> = HashMap::new();
    let mut titles = HashSet::new();
    for section in sections.iter().filter(|s| runnable(s)) {
        let task = section.title.as_str();
        if !titles.insert(task) {
            out.push(Diagnostic::error(
                Some(task),
                "defined more than once; only the first definition is used",
            ));
            continue;
        }
        for name in std::iter::once(task).chain(section.aliases.iter().map(String::as_str)) {
            match owner.get(name) {
                Some(&other) if other == task && name == task => {}
                Some(&other) => out.push(Diagnostic::error(
                    Some(task),
                    format!("the name '{}' is already used by task '{}'", name, other),
                )),
                None => {
                    owner.insert(name, task);
                }
            }
            if reserved.iter().any(|r| r == name) {
                out.push(Diagnostic::warning(
                    Some(task),
                    format!(
                        "the name '{}' is also a command; run the task with `mq-task run {}`",
                        name, name
                    ),
                ));
            }
        }
        let mut seen = HashSet::new();
        for param in &section.params {
            if !seen.insert(param.name.as_str()) {
                out.push(Diagnostic::error(
                    Some(task),
                    format!("parameter '{}' is declared twice", param.name),
                ));
            }
        }
    }
}

/// `depends`, `defer` and `for.from` that name no task.
fn check_references(sections: &[Section], failed: &[String], out: &mut Vec<Diagnostic>) {
    let names = || {
        sections
            .iter()
            .flat_map(|s| {
                std::iter::once(s.title.as_str()).chain(s.aliases.iter().map(String::as_str))
            })
            .chain(failed.iter().map(String::as_str))
    };
    let unknown = |spec: &str| {
        if resolve_call(sections, spec).is_some() {
            return None;
        }
        let first = spec.split_whitespace().next().unwrap_or(spec);
        if failed.iter().any(|f| f == spec || f == first) {
            return None;
        }
        let hint = closest(spec, names())
            .or_else(|| closest(first, names()))
            .map(|c| format!(" (did you mean `{}`?)", c))
            .unwrap_or_default();
        Some(hint)
    };

    for section in sections.iter().filter(|s| runnable(s)) {
        let task = Some(section.title.as_str());
        for spec in &section.depends {
            if let Some(hint) = unknown(spec) {
                out.push(Diagnostic::error(
                    task,
                    format!("depends on unknown task '{}'{}", spec, hint),
                ));
            }
        }
        for spec in &section.defer {
            if let Some(hint) = unknown(spec) {
                out.push(Diagnostic::error(
                    task,
                    format!("defers unknown task '{}'{}", spec, hint),
                ));
            }
        }
        if let Some(LoopSpec::From(source)) = &section.for_each
            && let Some(hint) = unknown(source)
        {
            out.push(Diagnostic::error(
                task,
                format!("`for.from` names unknown task '{}'{}", source, hint),
            ));
        }
    }
}

/// Dependency cycles, each reported once as `a -> b -> a`.
fn check_cycles(sections: &[Section], out: &mut Vec<Diagnostic>) {
    let index: HashMap<&str, usize> = sections
        .iter()
        .enumerate()
        .rev()
        .map(|(i, s)| (s.title.as_str(), i))
        .collect();
    let edges: Vec<Vec<usize>> = sections
        .iter()
        .map(|s| {
            s.depends
                .iter()
                .filter_map(|spec| resolve_call(sections, spec))
                .filter_map(|(target, _)| index.get(target.title.as_str()).copied())
                .collect()
        })
        .collect();

    fn walk(
        node: usize,
        edges: &[Vec<usize>],
        state: &mut [u8],
        path: &mut Vec<usize>,
        cycles: &mut Vec<Vec<usize>>,
    ) {
        state[node] = 1;
        path.push(node);
        for &next in &edges[node] {
            if state[next] == 1 {
                let start = path.iter().position(|&n| n == next).unwrap_or(0);
                cycles.push(path[start..].to_vec());
            } else if state[next] == 0 {
                walk(next, edges, state, path, cycles);
            }
        }
        path.pop();
        state[node] = 2;
    }

    let mut state = vec![0u8; sections.len()];
    let mut cycles = Vec::new();
    for node in 0..sections.len() {
        if state[node] == 0 {
            walk(node, &edges, &mut state, &mut Vec::new(), &mut cycles);
        }
    }

    let mut seen = HashSet::new();
    for cycle in cycles {
        let mut key = cycle.clone();
        key.sort_unstable();
        if !seen.insert(key) {
            continue;
        }
        let mut titles: Vec<&str> = cycle.iter().map(|&i| sections[i].title.as_str()).collect();
        titles.push(titles[0]);
        out.push(Diagnostic::error(
            Some(sections[cycle[0]].title.as_str()),
            format!("circular dependency: {}", titles.join(" -> ")),
        ));
    }
}

/// Whatever else planning each task reveals (`for.from` sources, deferred
/// tasks with dependencies). Missing tasks and cycles are reported above.
fn check_plans(sections: &[Section], out: &mut Vec<Diagnostic>) {
    let mut seen = HashSet::new();
    for section in sections.iter().filter(|s| runnable(s)) {
        if let Err(e) = build_plan(sections, &section.title, false)
            && !matches!(
                e,
                Error::SectionNotFound(_) | Error::CircularDependency(_) | Error::NoCommands(_)
            )
            && seen.insert(e.to_string())
        {
            out.push(Diagnostic::error(
                Some(section.title.as_str()),
                plain_message(&e),
            ));
        }
    }
}

/// Languages without a runtime, and runtimes that are not installed.
fn check_runtimes(sections: &[Section], config: &Config, out: &mut Vec<Diagnostic>) {
    let mut users: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for section in sections {
        for block in section.codes.iter().filter(|b| !b.lang.is_empty()) {
            let tasks = users.entry(block.lang.as_str()).or_default();
            if !tasks.contains(&section.title.as_str()) {
                tasks.push(section.title.as_str());
            }
        }
    }
    for (lang, tasks) in users {
        let used = format!("used by {}", quote_list(&tasks));
        match config.get_runtime(lang) {
            None => out.push(Diagnostic::error(
                tasks.first().copied(),
                format!(
                    "no runtime is configured for language '{}' ({}); add it under [runtimes] in mq-task.toml",
                    lang, used
                ),
            )),
            Some(command) => {
                let binary = command.split_whitespace().next().unwrap_or(command);
                if which::which(binary).is_err() {
                    out.push(Diagnostic::warning(
                        tasks.first().copied(),
                        format!(
                            "runtime '{}' for language '{}' was not found in PATH ({})",
                            binary, lang, used
                        ),
                    ));
                }
            }
        }
    }
}

fn quote_list(tasks: &[&str]) -> String {
    tasks
        .iter()
        .map(|t| format!("'{}'", t))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Glob patterns that cannot be compiled.
fn check_patterns(sections: &[Section], out: &mut Vec<Diagnostic>) {
    for section in sections.iter().filter(|s| runnable(s)) {
        let looped = match &section.for_each {
            Some(LoopSpec::Glob(patterns)) => patterns.as_slice(),
            _ => &[],
        };
        for (what, pattern) in section
            .sources
            .iter()
            .map(|p| ("sources", p))
            .chain(section.outputs.iter().map(|p| ("outputs", p)))
            .chain(looped.iter().map(|p| ("for", p)))
        {
            // `sources` and `outputs` take `!pattern` to exclude files.
            let glob = match what {
                "for" => pattern.as_str(),
                _ => pattern.strip_prefix('!').unwrap_or(pattern),
            };
            if let Err(e) = glob::Pattern::new(glob) {
                out.push(Diagnostic::error(
                    Some(section.title.as_str()),
                    format!(
                        "invalid glob pattern '{}' in `{}`: {}",
                        pattern, what, e.msg
                    ),
                ));
            }
        }
    }
}

/// `if` expressions that do not parse or evaluate.
fn check_conditions(sections: &[Section], out: &mut Vec<Diagnostic>) {
    let env = HashMap::new();
    let ctx = CondContext {
        env: &env,
        dir: None,
    };
    for section in sections.iter().filter(|s| runnable(s)) {
        if let Some(cond) = &section.condition
            && let Err(e) = condition::evaluate(cond, &ctx)
        {
            out.push(Diagnostic::error(
                Some(section.title.as_str()),
                plain_message(&e),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{CodeBlock, ParamDef};

    fn task(title: &str, depends: &[&str]) -> Section {
        Section {
            title: title.into(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            codes: vec![CodeBlock {
                lang: "bash".into(),
                code: "true".into(),
            }],
            ..Default::default()
        }
    }

    fn check(sections: &[Section]) -> Vec<Diagnostic> {
        check_sections(sections, &[], &Config::default(), &["run".to_string()])
    }

    fn messages(diagnostics: &[Diagnostic]) -> Vec<String> {
        diagnostics.iter().map(|d| d.message.clone()).collect()
    }

    #[test]
    fn a_clean_file_has_no_diagnostics() {
        let sections = vec![task("build", &[]), task("test", &["build"])];
        let found: Vec<_> = check(&sections)
            .into_iter()
            // `bash` may be missing on an unusual machine; that is not what is tested.
            .filter(|d| !d.message.contains("not found in PATH"))
            .collect();
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn unknown_dependencies_are_reported_with_a_suggestion() {
        let mut deploy = task("deploy", &["buil", "ghost x=1"]);
        deploy.defer = vec!["clenup".into()];
        deploy.for_each = Some(LoopSpec::From("lst".into()));
        let sections = vec![
            deploy,
            task("build", &[]),
            task("cleanup", &[]),
            task("list", &[]),
        ];
        let found = check(&sections);
        let msgs = messages(&found);
        assert!(
            msgs.contains(&"depends on unknown task 'buil' (did you mean `build`?)".to_string()),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"depends on unknown task 'ghost x=1'".to_string()),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"defers unknown task 'clenup' (did you mean `cleanup`?)".to_string()),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(
                &"`for.from` names unknown task 'lst' (did you mean `list`?)".to_string()
            ),
            "{msgs:?}"
        );
        assert!(found.iter().all(|d| d.task.as_deref() == Some("deploy")));
    }

    #[test]
    fn calls_with_arguments_and_aliases_resolve() {
        let mut build = task("build", &[]);
        build.aliases = vec!["b".into()];
        let sections = vec![build, task("ci", &["b release", "build env=prod"])];
        let found = check(&sections);
        assert!(
            !messages(&found).iter().any(|m| m.contains("unknown task")),
            "{found:?}"
        );
    }

    #[test]
    fn references_to_tasks_that_failed_to_parse_are_not_repeated() {
        let sections = vec![task("ci", &["broken"])];
        let found = check_sections(&sections, &["broken".to_string()], &Config::default(), &[]);
        assert!(!messages(&found).iter().any(|m| m.contains("unknown task")));
    }

    #[test]
    fn cycles_are_reported_once_with_their_path() {
        let sections = vec![
            task("a", &["b"]),
            task("b", &["c"]),
            task("c", &["a"]),
            task("self", &["self"]),
        ];
        let msgs = messages(&check(&sections));
        assert_eq!(
            msgs.iter().filter(|m| m.starts_with("circular")).count(),
            2,
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"circular dependency: a -> b -> c -> a".to_string()),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"circular dependency: self -> self".to_string()),
            "{msgs:?}"
        );
    }

    #[test]
    fn duplicate_names_and_parameters_are_errors() {
        let mut one = task("build", &[]);
        one.aliases = vec!["b".into(), "test".into()];
        one.params = vec![
            ParamDef {
                name: "env".into(),
                default: None,
            },
            ParamDef {
                name: "env".into(),
                default: None,
            },
        ];
        let mut two = task("test", &[]);
        two.aliases = vec!["b".into()];
        let sections = vec![one, two, task("build", &[])];
        let msgs = messages(&check(&sections));
        assert!(
            msgs.contains(&"the name 'test' is already used by task 'build'".to_string()),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"the name 'b' is already used by task 'build'".to_string()),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"defined more than once; only the first definition is used".to_string()),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"parameter 'env' is declared twice".to_string()),
            "{msgs:?}"
        );
    }

    #[test]
    fn names_that_are_commands_get_a_warning() {
        let mut t = task("run", &[]);
        t.aliases = vec!["go".into()];
        let found = check(&[t]);
        let warning = found
            .iter()
            .find(|d| d.severity == Severity::Warning && d.message.contains("also a command"))
            .expect("warning");
        assert!(warning.message.contains("mq-task run run"));
    }

    #[test]
    fn missing_runtimes_are_errors_and_uninstalled_ones_warnings() {
        let mut cobol = task("legacy", &[]);
        cobol.codes[0].lang = "cobol".into();
        let mut ghost = task("ghost", &[]);
        ghost.codes[0].lang = "ghostlang".into();
        let mut config = Config::default();
        config
            .apply_runtime_overrides(
                &["ghostlang:definitely-not-installed-xyz".to_string()],
                None,
            )
            .unwrap();
        let found = check_sections(&[cobol, ghost], &[], &config, &[]);
        let by = |sev| -> Vec<String> {
            found
                .iter()
                .filter(|d| d.severity == sev)
                .map(|d| d.message.clone())
                .collect()
        };
        assert_eq!(by(Severity::Error).len(), 1);
        assert!(by(Severity::Error)[0].contains("no runtime is configured for language 'cobol'"));
        assert!(by(Severity::Error)[0].contains("used by 'legacy'"));
        assert!(by(Severity::Warning)[0].contains("'definitely-not-installed-xyz'"));
    }

    #[test]
    fn invalid_globs_and_conditions_are_errors() {
        let mut t = task("t", &[]);
        t.sources = vec!["src/**/*.rs".into(), "[".into()];
        t.outputs = vec!["dist/[".into()];
        t.for_each = Some(LoopSpec::Glob(vec!["ok/*".into(), "bad[".into()]));
        t.condition = Some("???".into());
        let msgs = messages(&check(&[t]));
        for needle in [
            "invalid glob pattern '[' in `sources`",
            "invalid glob pattern 'dist/[' in `outputs`",
            "invalid glob pattern 'bad[' in `for`",
            "Invalid `if` condition '???'",
        ] {
            assert!(
                msgs.iter().any(|m| m.contains(needle)),
                "{needle}: {msgs:?}"
            );
        }
    }

    #[test]
    fn plan_errors_such_as_a_bad_for_from_source_are_reported_once() {
        let mut each = task("each", &["list"]);
        each.for_each = Some(LoopSpec::From("list".into()));
        // `list` does not capture its output.
        let sections = vec![task("list", &[]), each, task("ci", &["each"])];
        let msgs = messages(&check(&sections));
        let hits: Vec<_> = msgs
            .iter()
            .filter(|m| m.contains("capture = true"))
            .collect();
        assert_eq!(hits.len(), 1, "{msgs:?}");
    }

    #[test]
    fn headings_without_anything_to_run_are_ignored() {
        let title = Section {
            title: "Project".into(),
            ..Default::default()
        };
        assert!(
            check(&[title, task("a", &[])])
                .iter()
                .all(|d| d.task.as_deref() != Some("Project"))
        );
    }

    #[test]
    fn a_report_passes_unless_there_are_errors_or_strict_warnings() {
        let report = |diagnostics| Report {
            tasks: 1,
            diagnostics,
        };
        let warn = Diagnostic::warning(None, "w");
        let err = Diagnostic::error(None, "e");
        assert!(report(vec![]).passes(true));
        assert!(report(vec![warn.clone()]).passes(false));
        assert!(!report(vec![warn.clone()]).passes(true));
        assert!(!report(vec![err.clone(), warn]).passes(false));
        let r = report(vec![err.clone(), err]);
        assert_eq!((r.errors(), r.warnings()), (2, 0));
    }
}
