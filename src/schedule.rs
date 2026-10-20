//! Dependency planning and (parallel) execution of a task graph.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use crate::error::{Error, Result};
use crate::events::{RunObserver, TaskStatus};
use crate::exec::{Executor, NodeOutcome, NodeParams, TaskIo, output_var};
use crate::section::{LoopSpec, Section};

/// Look a task up by title or alias.
pub fn find_section<'a>(sections: &'a [Section], name: &str) -> Option<&'a Section> {
    sections
        .iter()
        .find(|s| s.title == name || s.aliases.iter().any(|a| a == name))
}

/// Split a task call such as `deploy env=prod "region=eu west"` into words.
/// Single and double quotes group words; there are no escapes.
pub fn split_words(spec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in spec.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

/// Resolve a call such as `deploy env=prod` (used by `depends` and `defer`)
/// into the task and its arguments.
///
/// The whole string is tried as a title first, so titles with spaces keep
/// working; otherwise the longest leading run of words naming a task wins.
pub fn resolve_call<'a>(sections: &'a [Section], spec: &str) -> Option<(&'a Section, Vec<String>)> {
    if let Some(section) = find_section(sections, spec.trim()) {
        return Some((section, Vec::new()));
    }
    let words = split_words(spec);
    (1..=words.len()).rev().find_map(|n| {
        find_section(sections, &words[..n].join(" ")).map(|s| (s, words[n..].to_vec()))
    })
}

/// Directories needed to expand `for` globs at plan time. Same precedence
/// as [`Executor::effective_dir`]: CLI `--dir`, task `dir`, document `dir`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanContext<'a> {
    pub working_dir: Option<&'a Path>,
    pub global_dir: Option<&'a Path>,
}

impl<'a> PlanContext<'a> {
    pub fn dir_for(&self, section: &'a Section) -> Option<&'a Path> {
        self.working_dir
            .or(section.dir.as_deref())
            .or(self.global_dir)
    }
}

/// A task whose code runs after another task's body, whatever its outcome.
#[derive(Debug, Clone)]
pub struct Deferred<'a> {
    pub section: &'a Section,
    pub args: Vec<String>,
}

/// One execution of a task: once per loop item, and once per distinct
/// argument list it is called with.
#[derive(Debug)]
pub struct PlanNode<'a> {
    pub section: &'a Section,
    /// Unique name: the title, plus `[...]` for call arguments or a loop item.
    pub label: String,
    /// Arguments from the dependent's call (the requested task uses the CLI's).
    pub args: Vec<String>,
    /// Loop variables of this iteration (`MX_ITEM`, `MX_INDEX`, ...).
    pub vars: Vec<(String, String)>,
    /// True for the task(s) the user asked to run.
    pub requested: bool,
    /// Reported as skipped with this reason instead of running.
    pub skip: Option<String>,
    /// Task whose captured output this node loops over (`for.from`).
    pub loop_source: Option<String>,
    /// Tasks to run once this one finishes (`defer`).
    pub defers: Vec<Deferred<'a>>,
    /// Declared dependencies (`depends`), as node indices.
    pub deps: Vec<usize>,
    /// Everything that must finish first: `deps` plus the ordering edges
    /// that keep non-`parallel` dependencies and loop items sequential.
    pub after: BTreeSet<usize>,
}

/// Tasks to run, ordered so every prerequisite precedes its dependents.
/// The requested task is always last (a looped one fills the last nodes).
#[derive(Debug)]
pub struct Plan<'a> {
    pub nodes: Vec<PlanNode<'a>>,
}

impl Plan<'_> {
    pub fn root(&self) -> usize {
        self.nodes.len() - 1
    }

    pub fn names(&self) -> Vec<String> {
        self.nodes.iter().map(|n| n.label.clone()).collect()
    }

    /// Every task paired with the labels of its declared dependencies.
    pub fn edges(&self) -> Vec<(String, Vec<String>)> {
        self.nodes
            .iter()
            .map(|n| {
                let deps = n
                    .deps
                    .iter()
                    .map(|&d| self.nodes[d].label.clone())
                    .collect();
                (n.label.clone(), deps)
            })
            .collect()
    }

    /// Transitive declared dependencies of `node` (excluding itself).
    fn ancestors(&self, node: usize) -> Vec<usize> {
        let mut seen = HashSet::new();
        let mut stack = self.nodes[node].deps.clone();
        let mut out = Vec::new();
        while let Some(i) = stack.pop() {
            if seen.insert(i) {
                out.push(i);
                stack.extend(self.nodes[i].deps.iter().copied());
            }
        }
        out.sort_unstable();
        out
    }
}

/// A node's label suffix (the loop item, if any) and its loop variables.
type Variant = (Option<String>, Vec<(String, String)>);

/// One iteration of a `for` loop.
struct Iteration {
    /// Text shown in the node label.
    label: String,
    vars: Vec<(String, String)>,
}

/// Name of the environment variable holding a loop variable.
pub fn loop_var(name: &str) -> String {
    let upper: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("MX_{}", upper)
}

/// The variables of one item of a list-like loop: `MX_ITEM`, `MX_INDEX` and
/// the task's `as` name.
pub fn loop_item_vars(section: &Section, index: usize, value: &str) -> Vec<(String, String)> {
    let mut vars = vec![
        ("MX_ITEM".to_string(), value.to_string()),
        ("MX_INDEX".to_string(), index.to_string()),
    ];
    if let Some(name) = section.loop_var.as_deref().map(loop_var)
        && name != "MX_ITEM"
    {
        vars.push((name, value.to_string()));
    }
    vars
}

/// Expand a task's `for` into its iterations.
fn expand_loop(section: &Section, spec: &LoopSpec, ctx: &PlanContext) -> Vec<Iteration> {
    let item = |index: usize, value: String| Iteration {
        vars: loop_item_vars(section, index, &value),
        label: value,
    };

    match spec {
        LoopSpec::List(items) => items
            .iter()
            .enumerate()
            .map(|(i, v)| item(i, v.clone()))
            .collect(),
        LoopSpec::Glob(patterns) => {
            let dir = ctx.dir_for(section);
            let mut found: Vec<String> = Vec::new();
            for pattern in patterns {
                let full = match dir {
                    Some(d) if !Path::new(pattern).is_absolute() => {
                        d.join(pattern).to_string_lossy().into_owned()
                    }
                    _ => pattern.clone(),
                };
                let Ok(paths) = glob::glob(&full) else {
                    continue;
                };
                for path in paths.flatten() {
                    // Items are relative to the directory the task runs in.
                    let rel = dir.and_then(|d| path.strip_prefix(d).ok()).unwrap_or(&path);
                    found.push(rel.to_string_lossy().into_owned());
                }
            }
            found.sort();
            found.dedup();
            found
                .into_iter()
                .enumerate()
                .map(|(i, v)| item(i, v))
                .collect()
        }
        // Resolved at run time.
        LoopSpec::From(_) => Vec::new(),
        LoopSpec::Matrix(axes) => {
            let mut combos: Vec<Vec<(String, String)>> = vec![Vec::new()];
            for (axis, values) in axes {
                combos = combos
                    .into_iter()
                    .flat_map(|prefix| {
                        values.iter().map(move |v| {
                            let mut next = prefix.clone();
                            next.push((axis.clone(), v.clone()));
                            next
                        })
                    })
                    .collect();
            }
            if axes.is_empty() {
                combos.clear();
            }
            combos
                .into_iter()
                .enumerate()
                .map(|(i, combo)| {
                    let label = combo
                        .iter()
                        .map(|(k, v)| format!("{}={}", k, v))
                        .collect::<Vec<_>>()
                        .join(",");
                    let mut vars = vec![("MX_INDEX".to_string(), i.to_string())];
                    vars.extend(combo.into_iter().map(|(k, v)| (loop_var(&k), v)));
                    Iteration { label, vars }
                })
                .collect()
        }
    }
}

/// Build the plan for `target`.
///
/// A task without `parallel` runs its dependencies one after another in
/// declaration order (every task newly pulled in by a later dependency waits
/// for the previous dependency). With `parallel` -- or `force_parallel` --
/// only the real dependency edges constrain the order. Loop items follow
/// the same rule.
pub fn build_plan<'a>(
    sections: &'a [Section],
    target: &str,
    force_parallel: bool,
) -> Result<Plan<'a>> {
    build_plan_in(sections, target, force_parallel, &PlanContext::default())
}

/// [`build_plan`] that resolves `for` globs against `ctx`.
pub fn build_plan_in<'a>(
    sections: &'a [Section],
    target: &str,
    force_parallel: bool,
    ctx: &PlanContext<'a>,
) -> Result<Plan<'a>> {
    struct Builder<'a, 'c> {
        sections: &'a [Section],
        force_parallel: bool,
        ctx: &'c PlanContext<'a>,
        nodes: Vec<PlanNode<'a>>,
        index: HashMap<String, Vec<usize>>,
        in_progress: HashSet<&'a str>,
    }

    impl<'a> Builder<'a, '_> {
        /// Plan the call `spec` and return its nodes (one per loop item).
        fn visit(&mut self, spec: &str) -> Result<Vec<usize>> {
            let (section, args) = resolve_call(self.sections, spec)
                .ok_or_else(|| Error::SectionNotFound(spec.to_string()))?;
            self.visit_section(section, args, spec)
        }

        /// Check that `for.from` names a capturing dependency; returns its title.
        fn check_loop_source(
            &self,
            section: &Section,
            source: &str,
            deps: &[usize],
        ) -> Result<String> {
            let task = find_section(self.sections, source).ok_or_else(|| {
                Error::Invalid(format!(
                    "task '{}': `for.from` names unknown task '{}'",
                    section.title, source
                ))
            })?;
            if !task.capture {
                return Err(Error::Invalid(format!(
                    "task '{}': `for.from` task '{}' must set `capture = true`",
                    section.title, task.title
                )));
            }
            let mut seen = HashSet::new();
            let mut stack: Vec<usize> = deps.to_vec();
            while let Some(i) = stack.pop() {
                if !seen.insert(i) {
                    continue;
                }
                if self.nodes[i].section.title == task.title {
                    return Ok(task.title.clone());
                }
                stack.extend(self.nodes[i].deps.iter().copied());
            }
            Err(Error::Invalid(format!(
                "task '{}': `for.from` task '{}' must be one of its dependencies",
                section.title, task.title
            )))
        }

        fn visit_section(
            &mut self,
            section: &'a Section,
            args: Vec<String>,
            spec: &str,
        ) -> Result<Vec<usize>> {
            let title: &'a str = &section.title;

            if self.in_progress.contains(title) {
                return Err(Error::CircularDependency(spec.to_string()));
            }
            let base_label = if args.is_empty() {
                title.to_string()
            } else {
                format!("{}[{}]", title, args.join(" "))
            };
            // A `once` task is shared by every call, whatever its arguments.
            let key = if section.once {
                title.to_string()
            } else {
                base_label.clone()
            };
            if let Some(ids) = self.index.get(&key) {
                return Ok(ids.clone());
            }

            self.in_progress.insert(title);
            let parallel = self.force_parallel || section.parallel;

            let mut deps = Vec::new();
            let mut after = BTreeSet::new();
            let mut prev: Vec<usize> = Vec::new();
            for dep in &section.depends {
                let start = self.nodes.len();
                let ids = self.visit(dep)?;
                if !parallel && !prev.is_empty() {
                    for node in &mut self.nodes[start..] {
                        node.after.extend(prev.iter().copied());
                    }
                }
                deps.extend(ids.iter().copied());
                after.extend(ids.iter().copied());
                prev = ids;
            }

            let mut defers = Vec::new();
            for spec in &section.defer {
                let (target, args) = resolve_call(self.sections, spec)
                    .ok_or_else(|| Error::SectionNotFound(spec.clone()))?;
                if !target.depends.is_empty() {
                    return Err(Error::Invalid(format!(
                        "task '{}': deferred task '{}' must not have dependencies",
                        section.title, target.title
                    )));
                }
                defers.push(Deferred {
                    section: target,
                    args,
                });
            }
            self.in_progress.remove(title);

            // One node per loop item; a plain task is a single node.
            let mut loop_source = None;
            let mut skip = None;
            let plain = || vec![(None, Vec::new())];
            let variants: Vec<Variant> = match &section.for_each {
                Some(LoopSpec::From(source)) => {
                    loop_source = Some(self.check_loop_source(section, source, &deps)?);
                    plain()
                }
                Some(spec) => {
                    let mut seen = HashSet::new();
                    let items: Vec<_> = expand_loop(section, spec, self.ctx)
                        .into_iter()
                        .filter(|it| seen.insert(it.label.clone()))
                        .map(|it| (Some(it.label), it.vars))
                        .collect();
                    if items.is_empty() {
                        skip = Some("for: no items".to_string());
                        plain()
                    } else {
                        items
                    }
                }
                None => plain(),
            };

            let mut ids: Vec<usize> = Vec::new();
            for (suffix, vars) in variants {
                let mut node_after = after.clone();
                if !parallel && let Some(&last) = ids.last() {
                    node_after.insert(last);
                }
                ids.push(self.nodes.len());
                self.nodes.push(PlanNode {
                    section,
                    label: match &suffix {
                        Some(item) => format!("{}[{}]", base_label, item),
                        None => base_label.clone(),
                    },
                    args: args.clone(),
                    vars,
                    requested: false,
                    skip: skip.clone(),
                    loop_source: loop_source.clone(),
                    defers: if skip.is_some() {
                        Vec::new()
                    } else {
                        defers.clone()
                    },
                    deps: deps.clone(),
                    after: node_after,
                });
            }
            self.index.insert(key, ids.clone());
            Ok(ids)
        }
    }

    let mut builder = Builder {
        sections,
        force_parallel,
        ctx,
        nodes: Vec::new(),
        index: HashMap::new(),
        in_progress: HashSet::new(),
    };
    let root =
        find_section(sections, target).ok_or_else(|| Error::SectionNotFound(target.to_string()))?;
    if root.codes.is_empty() && root.depends.is_empty() {
        return Err(Error::NoCommands(target.to_string()));
    }
    let ids = builder.visit_section(root, Vec::new(), target)?;
    for id in ids {
        builder.nodes[id].requested = true;
    }
    Ok(Plan {
        nodes: builder.nodes,
    })
}

/// How a plan is executed.
pub struct ScheduleOptions<'a> {
    pub args: &'a [String],
    pub lang_filter: Option<&'a str>,
    /// Maximum tasks running at once; 1 runs sequentially with the terminal
    /// attached directly to each task.
    pub jobs: usize,
    /// Keep running independent tasks after a failure.
    pub keep_going: bool,
    /// Route task output through the observer even when running sequentially.
    pub piped: bool,
    /// Raised by the caller to abort the run (kills running processes).
    pub cancel: &'a AtomicBool,
    pub observer: &'a dyn RunObserver,
}

/// Execute `plan`. Returns the first task error, if any.
pub fn run_plan(exec: &Executor, plan: &Plan, opts: &ScheduleOptions) -> Result<()> {
    let n = plan.nodes.len();
    let jobs = opts.jobs.max(1);
    let piped = opts.piped || jobs > 1;
    let cancel = opts.cancel;
    let ancestors: Vec<Vec<usize>> = (0..n).map(|i| plan.ancestors(i)).collect();

    // Captured stdout per node, exposed to dependents as MX_OUTPUT_<TASK>.
    // A task that ran several times has its outputs joined by newlines.
    let mut outputs: Vec<Option<String>> = vec![None; n];
    let extra_env_for = |i: usize, outputs: &[Option<String>]| -> Vec<(String, String)> {
        let mut grouped: Vec<(String, String)> = Vec::new();
        for &a in &ancestors[i] {
            let Some(out) = &outputs[a] else { continue };
            let var = output_var(&plan.nodes[a].section.title);
            match grouped.iter_mut().find(|(k, _)| *k == var) {
                Some((_, joined)) => {
                    joined.push('\n');
                    joined.push_str(out);
                }
                None => grouped.push((var, out.clone())),
            }
        }
        grouped.extend(plan.nodes[i].vars.iter().cloned());
        grouped
    };

    let run_one = |i: usize, extra_env: Vec<(String, String)>| -> Result<NodeOutcome> {
        let node = &plan.nodes[i];
        let section = node.section;
        if let Some(reason) = &node.skip {
            opts.observer.status(
                &node.label,
                &TaskStatus::Started {
                    dependency: !node.requested,
                },
            );
            opts.observer.status(
                &node.label,
                &TaskStatus::Skipped {
                    reason: reason.clone(),
                },
            );
            return Ok(NodeOutcome::Skipped);
        }
        let io = TaskIo {
            name: &node.label,
            observer: opts.observer,
            piped,
            capture: section.capture,
            cancel,
            deadline: None,
        };
        exec.run_node(
            &NodeParams {
                section,
                label: &node.label,
                // The requested task takes the CLI arguments.
                args: if node.requested {
                    opts.args
                } else {
                    &node.args
                },
                lang_filter: opts.lang_filter,
                extra_env,
                dependency: !node.requested,
                loop_source: node.loop_source.as_deref(),
                defers: &node.defers,
            },
            &io,
        )
    };

    let mut first_err: Option<Error> = None;

    if jobs == 1 {
        let mut blocked = vec![false; n];
        // Tell the observer that every task from `from` on was not run.
        let report_unreached = |from: usize, reason: &str| {
            for node in &plan.nodes[from..] {
                opts.observer.status(
                    &node.label,
                    &TaskStatus::Cancelled {
                        reason: reason.into(),
                    },
                );
            }
        };
        for i in 0..n {
            let name = &plan.nodes[i].label;
            if cancel.load(Ordering::SeqCst) {
                report_unreached(i, "aborted");
                return Err(first_err.take().unwrap_or(Error::Cancelled));
            }
            // Only a failed *declared* dependency blocks a task. Mere
            // ordering (a sibling that happens to come earlier) does not.
            if plan.nodes[i].deps.iter().any(|&d| blocked[d]) {
                blocked[i] = true;
                opts.observer.status(
                    name,
                    &TaskStatus::Cancelled {
                        reason: "a dependency failed".into(),
                    },
                );
                continue;
            }
            match run_one(i, extra_env_for(i, &outputs)) {
                Ok(NodeOutcome::Done { output }) => outputs[i] = output,
                Ok(NodeOutcome::Skipped) => {}
                Err(e) => {
                    blocked[i] = true;
                    if !opts.keep_going {
                        report_unreached(i + 1, "a dependency failed or the run was aborted");
                        return Err(e);
                    }
                    first_err.get_or_insert(e);
                }
            }
        }
        return first_err.map_or(Ok(()), Err);
    }

    let mut remaining: Vec<usize> = plan.nodes.iter().map(|nd| nd.after.len()).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, node) in plan.nodes.iter().enumerate() {
        for &d in &node.after {
            dependents[d].push(i);
        }
    }
    let mut ready: BTreeSet<usize> = (0..n).filter(|&i| remaining[i] == 0).collect();
    let mut started = vec![false; n];
    // A declared dependency failed (or was itself not run): never start.
    let mut tainted = vec![false; n];
    let mut running = 0usize;
    let mut stop = false;

    std::thread::scope(|s| {
        let (tx, rx) = mpsc::channel::<(usize, Result<NodeOutcome>)>();
        loop {
            while !stop && running < jobs {
                let Some(i) = ready.pop_first() else { break };
                started[i] = true;
                running += 1;
                let tx = tx.clone();
                let extra_env = extra_env_for(i, &outputs);
                let run_one = &run_one;
                s.spawn(move || {
                    let _ = tx.send((i, run_one(i, extra_env)));
                });
            }
            if running == 0 {
                break;
            }
            let Ok((i, result)) = rx.recv() else { break };
            running -= 1;
            let failed = match result {
                Ok(outcome) => {
                    if let NodeOutcome::Done { output } = outcome {
                        outputs[i] = output;
                    }
                    false
                }
                Err(e) => {
                    if !matches!(e, Error::Cancelled) {
                        first_err.get_or_insert(e);
                    }
                    if !opts.keep_going {
                        stop = true;
                        cancel.store(true, Ordering::SeqCst);
                    }
                    true
                }
            };

            // Release everything that was waiting on `i`. Ordering edges
            // only wait for it to finish; a failure additionally taints
            // the tasks that declare it as a dependency, and those never
            // start, which in turn taints their own dependents.
            let mut settled = vec![(i, failed)];
            while let Some((done, bad)) = settled.pop() {
                for &d in &dependents[done] {
                    if bad && plan.nodes[d].deps.contains(&done) {
                        tainted[d] = true;
                    }
                    remaining[d] -= 1;
                    if remaining[d] == 0 {
                        if tainted[d] {
                            settled.push((d, true));
                        } else {
                            ready.insert(d);
                        }
                    }
                }
            }
        }
    });

    for (i, node) in plan.nodes.iter().enumerate() {
        if !started[i] {
            opts.observer.status(
                &node.label,
                &TaskStatus::Cancelled {
                    reason: if first_err.is_some() {
                        "a dependency failed or the run was aborted".into()
                    } else {
                        "not reached".into()
                    },
                },
            );
        }
    }

    first_err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(title: &str, depends: &[&str], parallel: bool) -> Section {
        Section {
            title: title.to_string(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            parallel,
            ..Default::default()
        }
    }

    fn names(plan: &Plan) -> Vec<String> {
        plan.names()
    }

    #[test]
    fn orders_dependencies_before_dependents() {
        let sections = vec![
            sec("a", &[], false),
            sec("b", &["a"], false),
            sec("c", &["a", "b"], false),
        ];
        let plan = build_plan(&sections, "c", false).unwrap();
        assert_eq!(names(&plan), vec!["a", "b", "c"]);
    }

    #[test]
    fn detects_cycles_and_missing_tasks() {
        let sections = vec![sec("a", &["b"], false), sec("b", &["a"], false)];
        assert!(matches!(
            build_plan(&sections, "a", false),
            Err(Error::CircularDependency(_))
        ));
        assert!(matches!(
            build_plan(&sections, "zzz", false),
            Err(Error::SectionNotFound(_))
        ));
    }

    #[test]
    fn rejects_a_target_with_nothing_to_run() {
        let sections = vec![sec("a", &[], false)];
        assert!(matches!(
            build_plan(&sections, "a", false),
            Err(Error::NoCommands(_))
        ));
    }

    #[test]
    fn sequential_tasks_chain_their_dependencies() {
        let sections = vec![
            sec("a", &[], false),
            sec("b", &[], false),
            sec("ci", &["a", "b"], false),
        ];
        let plan = build_plan(&sections, "ci", false).unwrap();
        let b = plan
            .nodes
            .iter()
            .position(|n| n.section.title == "b")
            .unwrap();
        let a = plan
            .nodes
            .iter()
            .position(|n| n.section.title == "a")
            .unwrap();
        assert!(plan.nodes[b].after.contains(&a));
    }

    #[test]
    fn parallel_tasks_leave_dependencies_unordered() {
        let sections = vec![
            sec("a", &[], false),
            sec("b", &[], false),
            sec("ci", &["a", "b"], true),
        ];
        let plan = build_plan(&sections, "ci", false).unwrap();
        let b = plan
            .nodes
            .iter()
            .position(|n| n.section.title == "b")
            .unwrap();
        assert!(plan.nodes[b].after.is_empty());

        // The CLI flag forces parallel regardless of the task's own setting.
        let sections = vec![
            sec("a", &[], false),
            sec("b", &[], false),
            sec("ci", &["a", "b"], false),
        ];
        let plan = build_plan(&sections, "ci", true).unwrap();
        let b = plan
            .nodes
            .iter()
            .position(|n| n.section.title == "b")
            .unwrap();
        assert!(plan.nodes[b].after.is_empty());
    }

    #[test]
    fn sequential_mode_orders_whole_subtrees() {
        // release: [build, migrate]; migrate: [backup]. Sequentially backup
        // must not start before build finishes.
        let sections = vec![
            sec("build", &[], false),
            sec("backup", &[], false),
            sec("migrate", &["backup"], false),
            sec("release", &["build", "migrate"], false),
        ];
        let plan = build_plan(&sections, "release", false).unwrap();
        let idx = |t: &str| {
            plan.nodes
                .iter()
                .position(|n| n.section.title == t)
                .unwrap()
        };
        assert!(plan.nodes[idx("backup")].after.contains(&idx("build")));
        assert!(plan.nodes[idx("migrate")].after.contains(&idx("build")));
    }

    #[test]
    fn shared_dependencies_appear_once() {
        let sections = vec![
            sec("base", &[], false),
            sec("x", &["base"], true),
            sec("y", &["base"], true),
            sec("all", &["x", "y"], true),
        ];
        let plan = build_plan(&sections, "all", false).unwrap();
        assert_eq!(plan.nodes.len(), 4);
    }

    #[test]
    fn conflicting_orders_do_not_create_cycles() {
        // x orders [a, b] but y orders [b, a]; first visit wins.
        let sections = vec![
            sec("a", &[], false),
            sec("b", &[], false),
            sec("x", &["a", "b"], false),
            sec("y", &["b", "a"], false),
            sec("root", &["x", "y"], false),
        ];
        assert!(build_plan(&sections, "root", false).is_ok());
    }

    fn looped(title: &str, spec: LoopSpec) -> Section {
        Section {
            for_each: Some(spec),
            ..sec(title, &[], false)
        }
    }

    fn list(items: &[&str]) -> LoopSpec {
        LoopSpec::List(items.iter().map(|s| s.to_string()).collect())
    }

    fn with_code(mut s: Section) -> Section {
        s.codes = vec![crate::section::CodeBlock {
            lang: "bash".into(),
            code: "true".into(),
        }];
        s
    }

    #[test]
    fn splits_words_with_quotes() {
        assert_eq!(split_words("  a  b "), vec!["a", "b"]);
        assert_eq!(
            split_words(r#"deploy env=prod "region=eu west" 'x y'"#),
            vec!["deploy", "env=prod", "region=eu west", "x y"]
        );
        assert_eq!(split_words(r#"a "" b"#), vec!["a", "", "b"]);
        assert!(split_words("   ").is_empty());
    }

    #[test]
    fn resolves_calls_with_arguments() {
        let sections = vec![
            sec("build", &[], false),
            sec("Build App", &[], false),
            sec("Build", &[], false),
        ];
        let (s, args) = resolve_call(&sections, "build env=prod").unwrap();
        assert_eq!((s.title.as_str(), args), ("build", vec!["env=prod".into()]));

        // The whole string is tried as a title first.
        let (s, args) = resolve_call(&sections, "Build App").unwrap();
        assert_eq!(s.title, "Build App");
        assert!(args.is_empty());

        // Otherwise the longest matching prefix wins.
        let (s, args) = resolve_call(&sections, "Build App x=1").unwrap();
        assert_eq!((s.title.as_str(), args), ("Build App", vec!["x=1".into()]));
        let (s, args) = resolve_call(&sections, "Build other").unwrap();
        assert_eq!((s.title.as_str(), args), ("Build", vec!["other".into()]));

        assert!(resolve_call(&sections, "nope x").is_none());
    }

    #[test]
    fn a_loop_becomes_one_node_per_item_in_order() {
        let sections = vec![with_code(looped("t", list(&["a", "b", "c"])))];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert_eq!(names(&plan), vec!["t[a]", "t[b]", "t[c]"]);
        assert!(plan.nodes.iter().all(|n| n.requested));
        assert!(plan.nodes[0].after.is_empty());
        assert_eq!(plan.nodes[1].after, BTreeSet::from([0]));
        assert_eq!(plan.nodes[2].after, BTreeSet::from([1]));
        assert_eq!(
            plan.nodes[1].vars,
            vec![
                ("MX_ITEM".to_string(), "b".to_string()),
                ("MX_INDEX".to_string(), "1".to_string())
            ]
        );
    }

    #[test]
    fn parallel_loops_leave_iterations_unordered() {
        let mut s = with_code(looped("t", list(&["a", "b"])));
        s.parallel = true;
        let sections = vec![s];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert!(plan.nodes.iter().all(|n| n.after.is_empty()));

        let sections = vec![with_code(looped("t", list(&["a", "b"])))];
        let plan = build_plan(&sections, "t", true).unwrap();
        assert!(plan.nodes.iter().all(|n| n.after.is_empty()));
    }

    #[test]
    fn loop_aliases_and_duplicate_items() {
        let mut s = with_code(looped("t", list(&["x", "x", "y"])));
        s.loop_var = Some("file-name".into());
        let sections = vec![s];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert_eq!(names(&plan), vec!["t[x]", "t[y]"]);
        assert!(
            plan.nodes[0]
                .vars
                .contains(&("MX_FILE_NAME".to_string(), "x".to_string()))
        );
    }

    #[test]
    fn matrix_is_a_cross_product_with_named_variables() {
        let spec = LoopSpec::Matrix(vec![
            ("arch".into(), vec!["x64".into(), "arm".into()]),
            ("os".into(), vec!["linux".into(), "mac".into()]),
        ]);
        let sections = vec![with_code(looped("t", spec))];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert_eq!(
            names(&plan),
            vec![
                "t[arch=x64,os=linux]",
                "t[arch=x64,os=mac]",
                "t[arch=arm,os=linux]",
                "t[arch=arm,os=mac]"
            ]
        );
        assert!(
            plan.nodes[1]
                .vars
                .contains(&("MX_OS".to_string(), "mac".to_string()))
        );
        assert!(
            plan.nodes[1]
                .vars
                .contains(&("MX_ARCH".to_string(), "x64".to_string()))
        );
    }

    #[test]
    fn an_empty_loop_is_a_single_skipped_node() {
        let sections = vec![with_code(looped("t", list(&[])))];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert_eq!(names(&plan), vec!["t"]);
        assert!(plan.nodes[0].skip.is_some());
    }

    #[test]
    fn dependents_wait_for_every_iteration() {
        let sections = vec![
            with_code(looped("each", list(&["a", "b"]))),
            with_code(Section {
                depends: vec!["each".into()],
                ..sec("all", &[], false)
            }),
        ];
        let plan = build_plan(&sections, "all", false).unwrap();
        assert_eq!(names(&plan), vec!["each[a]", "each[b]", "all"]);
        assert_eq!(plan.nodes[2].deps, vec![0, 1]);
        assert!(plan.nodes[2].requested);
        assert!(!plan.nodes[0].requested);
    }

    #[test]
    fn globs_expand_relative_to_the_task_directory() {
        let dir = std::env::temp_dir().join(format!("mq-task-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.txt"), "").unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join("c.md"), "").unwrap();

        let mut s = with_code(looped(
            "t",
            LoopSpec::Glob(vec!["*.txt".into(), "s*".into()]),
        ));
        s.dir = Some(dir.clone());
        let sections = vec![s];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert_eq!(names(&plan), vec!["t[a.txt]", "t[b.txt]", "t[sub]"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calls_with_different_arguments_are_separate_nodes() {
        let sections = vec![
            with_code(sec("build", &[], false)),
            with_code(sec(
                "ci",
                &["build env=a", "build env=b", "build env=a"],
                false,
            )),
        ];
        let plan = build_plan(&sections, "ci", false).unwrap();
        assert_eq!(names(&plan), vec!["build[env=a]", "build[env=b]", "ci"]);
        assert_eq!(plan.nodes[0].args, vec!["env=a"]);
        assert_eq!(plan.nodes[1].args, vec!["env=b"]);
        assert_eq!(plan.nodes[2].deps, vec![0, 1, 0]);
        // Sequential: the second call waits for the first.
        assert!(plan.nodes[1].after.contains(&0));
    }

    #[test]
    fn a_once_task_is_shared_by_calls_with_different_arguments() {
        let mut build = with_code(sec("build", &[], false));
        build.once = true;
        let sections = vec![
            build,
            with_code(sec("ci", &["build env=a", "build env=b"], false)),
        ];
        let plan = build_plan(&sections, "ci", false).unwrap();
        // The first call wins.
        assert_eq!(names(&plan), vec!["build[env=a]", "ci"]);
        assert_eq!(plan.nodes[1].deps, vec![0, 0]);
    }

    fn from_loop(title: &str, source: &str, depends: &[&str]) -> Section {
        Section {
            for_each: Some(LoopSpec::From(source.into())),
            ..with_code(sec(title, depends, false))
        }
    }

    fn capturing(title: &str) -> Section {
        Section {
            capture: true,
            ..with_code(sec(title, &[], false))
        }
    }

    #[test]
    fn a_from_loop_is_one_node_that_knows_its_source() {
        let mut list = capturing("list");
        list.aliases = vec!["ls".into()];
        let sections = vec![list, from_loop("each", "ls", &["list"])];
        let plan = build_plan(&sections, "each", false).unwrap();
        assert_eq!(names(&plan), vec!["list", "each"]);
        // The alias is resolved to the task's title.
        assert_eq!(plan.nodes[1].loop_source.as_deref(), Some("list"));
        assert_eq!(plan.nodes[0].loop_source, None);
    }

    #[test]
    fn a_from_source_may_be_a_transitive_dependency() {
        let sections = vec![
            capturing("list"),
            with_code(sec("wrapper", &["list"], false)),
            from_loop("each", "list", &["wrapper"]),
        ];
        assert!(build_plan(&sections, "each", false).is_ok());
    }

    #[test]
    fn a_from_source_must_exist_capture_and_be_a_dependency() {
        let unknown = vec![from_loop("each", "nope", &[])];
        assert!(matches!(
            build_plan(&unknown, "each", false),
            Err(Error::Invalid(m)) if m.contains("unknown task 'nope'")
        ));

        let no_capture = vec![
            with_code(sec("list", &[], false)),
            from_loop("each", "list", &["list"]),
        ];
        assert!(matches!(
            build_plan(&no_capture, "each", false),
            Err(Error::Invalid(m)) if m.contains("capture = true")
        ));

        let not_a_dependency = vec![capturing("list"), from_loop("each", "list", &[])];
        assert!(matches!(
            build_plan(&not_a_dependency, "each", false),
            Err(Error::Invalid(m)) if m.contains("one of its dependencies")
        ));
    }

    #[test]
    fn unknown_calls_report_the_whole_spec() {
        let sections = vec![with_code(sec("a", &["ghost x=1"], false))];
        let err = build_plan(&sections, "a", false).unwrap_err();
        assert!(matches!(err, Error::SectionNotFound(s) if s == "ghost x=1"));
    }

    #[test]
    fn defers_resolve_to_tasks_without_dependencies() {
        let mut main = with_code(sec("main", &[], false));
        main.defer = vec!["cleanup dir=/tmp/x".into()];
        let sections = vec![main.clone(), with_code(sec("cleanup", &[], false))];
        let plan = build_plan(&sections, "main", false).unwrap();
        assert_eq!(plan.nodes[0].defers.len(), 1);
        assert_eq!(plan.nodes[0].defers[0].section.title, "cleanup");
        assert_eq!(plan.nodes[0].defers[0].args, vec!["dir=/tmp/x"]);
        // A deferred task is not part of the plan itself.
        assert_eq!(plan.nodes.len(), 1);

        let sections = vec![main.clone()];
        assert!(matches!(
            build_plan(&sections, "main", false),
            Err(Error::SectionNotFound(_))
        ));

        let sections = vec![
            main,
            with_code(sec("cleanup", &["other"], false)),
            with_code(sec("other", &[], false)),
        ];
        assert!(matches!(
            build_plan(&sections, "main", false),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn split_words_is_lenient_about_unbalanced_quotes() {
        assert_eq!(split_words(r#"a "b c"#), vec!["a", "b c"]);
        assert_eq!(split_words("it's"), vec!["its"]);
        assert_eq!(split_words(r#"x="a b" y"#), vec!["x=a b", "y"]);
    }

    #[test]
    fn loop_item_vars_name_the_item_once() {
        let mut s = sec("t", &[], false);
        let names = |s: &Section| -> Vec<String> {
            loop_item_vars(s, 2, "v")
                .into_iter()
                .map(|(k, _)| k)
                .collect()
        };
        assert_eq!(names(&s), vec!["MX_ITEM", "MX_INDEX"]);

        s.loop_var = Some("file".into());
        assert_eq!(names(&s), vec!["MX_ITEM", "MX_INDEX", "MX_FILE"]);

        // `as = "item"` is the default name, not a duplicate.
        s.loop_var = Some("item".into());
        assert_eq!(names(&s), vec!["MX_ITEM", "MX_INDEX"]);
        assert_eq!(loop_var("my-axis.x"), "MX_MY_AXIS_X");
    }

    #[test]
    fn plan_context_prefers_cli_then_task_then_document_dir() {
        let mut s = sec("t", &[], false);
        let cli = Path::new("cli");
        let doc = Path::new("doc");
        let ctx = |working_dir, global_dir| PlanContext {
            working_dir,
            global_dir,
        };
        assert_eq!(ctx(None, None).dir_for(&s), None);
        assert_eq!(ctx(None, Some(doc)).dir_for(&s), Some(doc));
        s.dir = Some("task".into());
        assert_eq!(ctx(None, Some(doc)).dir_for(&s), Some(Path::new("task")));
        assert_eq!(ctx(Some(cli), Some(doc)).dir_for(&s), Some(cli));
    }

    #[test]
    fn glob_loops_accept_absolute_patterns_and_ignore_invalid_ones() {
        let dir = std::env::temp_dir().join(format!("mq-task-abs-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();

        let pattern = dir.join("*.txt").to_string_lossy().into_owned();
        let spec = LoopSpec::Glob(vec![pattern.clone(), "[".into()]);
        let sections = vec![with_code(looped("t", spec))];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(
            plan.nodes[0].label,
            format!("t[{}]", dir.join("a.txt").display())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_matrix_axes_make_an_empty_loop() {
        let none = LoopSpec::Matrix(Vec::new());
        let sections = vec![with_code(looped("t", none))];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert!(plan.nodes[0].skip.is_some());

        let empty_axis = LoopSpec::Matrix(vec![("os".into(), Vec::new())]);
        let sections = vec![with_code(looped("t", empty_axis))];
        let plan = build_plan(&sections, "t", false).unwrap();
        assert!(plan.nodes[0].skip.is_some());
    }

    #[test]
    fn an_empty_loop_drops_its_defers_and_keeps_dependencies() {
        let mut each = with_code(looped("each", list(&[])));
        each.depends = vec!["base".into()];
        each.defer = vec!["cleanup".into()];
        let sections = vec![
            with_code(sec("base", &[], false)),
            with_code(sec("cleanup", &[], false)),
            each,
        ];
        let plan = build_plan(&sections, "each", false).unwrap();
        let node = plan.nodes.last().unwrap();
        assert!(node.skip.is_some() && node.defers.is_empty());
        assert_eq!(node.deps, vec![0]);
    }

    #[test]
    fn loop_items_inherit_defers_and_dependencies() {
        let mut each = with_code(looped("each", list(&["a", "b"])));
        each.depends = vec!["base".into()];
        each.defer = vec!["cleanup".into()];
        let sections = vec![
            with_code(sec("base", &[], false)),
            with_code(sec("cleanup", &[], false)),
            each,
        ];
        let plan = build_plan(&sections, "each", false).unwrap();
        assert_eq!(names(&plan), vec!["base", "each[a]", "each[b]"]);
        for node in &plan.nodes[1..] {
            assert_eq!(node.defers.len(), 1);
            assert_eq!(node.deps, vec![0]);
        }
    }

    #[test]
    fn edges_use_labels() {
        let sections = vec![
            with_code(sec("build", &[], false)),
            with_code(sec("ci", &["build env=a"], false)),
        ];
        let plan = build_plan(&sections, "ci", false).unwrap();
        assert_eq!(
            plan.edges(),
            vec![
                ("build[env=a]".to_string(), vec![]),
                ("ci".to_string(), vec!["build[env=a]".to_string()])
            ]
        );
    }
}
