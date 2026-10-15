//! Dependency planning and (parallel) execution of a task graph.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use crate::error::{Error, Result};
use crate::events::{RunObserver, TaskStatus};
use crate::exec::{Executor, NodeOutcome, NodeParams, TaskIo, output_var};
use crate::runner::Section;

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

/// Resolve a task call -- a task name optionally followed by arguments, as
/// used by `depends` -- into the task and its arguments.
///
/// The whole string is tried as a task name first, so titles containing
/// spaces keep working; otherwise the longest leading run of words that names
/// a task wins and the remaining words are its arguments.
pub fn resolve_call<'a>(sections: &'a [Section], spec: &str) -> Option<(&'a Section, Vec<String>)> {
    if let Some(section) = find_section(sections, spec.trim()) {
        return Some((section, Vec::new()));
    }
    let words = split_words(spec);
    (1..=words.len()).rev().find_map(|n| {
        find_section(sections, &words[..n].join(" ")).map(|s| (s, words[n..].to_vec()))
    })
}

/// One task execution in a plan. A task normally appears once; a task
/// depended upon with different arguments appears once per distinct argument
/// list.
#[derive(Debug)]
pub struct PlanNode<'a> {
    pub section: &'a Section,
    /// Unique display name: the task title, followed by `[...]` when the
    /// node was called with arguments.
    pub label: String,
    /// Arguments the task was called with by its dependent. The requested
    /// task takes its arguments from the command line instead.
    pub args: Vec<String>,
    /// True for the task the user asked to run.
    pub requested: bool,
    /// Declared dependencies (`depends`), as node indices.
    pub deps: Vec<usize>,
    /// Everything that must finish before this node starts: the declared
    /// dependencies plus ordering edges that keep non-`parallel` tasks'
    /// dependencies sequential.
    pub after: BTreeSet<usize>,
}

/// Tasks to run, ordered so every prerequisite precedes its dependents.
/// The requested task is always the last node.
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

/// Build the plan for `target`.
///
/// A task without `parallel` runs its dependencies one after another in
/// declaration order (every task newly pulled in by a later dependency waits
/// for the previous dependency). With `parallel` -- or `force_parallel` --
/// only the real dependency edges constrain the order.
pub fn build_plan<'a>(
    sections: &'a [Section],
    target: &str,
    force_parallel: bool,
) -> Result<Plan<'a>> {
    struct Builder<'a> {
        sections: &'a [Section],
        force_parallel: bool,
        nodes: Vec<PlanNode<'a>>,
        index: HashMap<String, Vec<usize>>,
        in_progress: HashSet<&'a str>,
    }

    impl<'a> Builder<'a> {
        /// Plan the call `spec` (a task name with optional arguments) and
        /// return the nodes that represent it: one, or one per iteration.
        fn visit(&mut self, spec: &str) -> Result<Vec<usize>> {
            let (section, args) = resolve_call(self.sections, spec)
                .ok_or_else(|| Error::SectionNotFound(spec.to_string()))?;
            self.visit_section(section, args, spec)
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
            if let Some(ids) = self.index.get(&base_label) {
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

            self.in_progress.remove(title);

            let id = self.nodes.len();
            self.nodes.push(PlanNode {
                section,
                label: base_label.clone(),
                args,
                requested: false,
                deps,
                after,
            });
            let ids = vec![id];
            self.index.insert(base_label, ids.clone());
            Ok(ids)
        }
    }

    let mut builder = Builder {
        sections,
        force_parallel,
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
    // When a task ran several times (with different arguments) the outputs
    // are joined with newlines in plan order.
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
        grouped
    };

    let run_one = |i: usize, extra_env: Vec<(String, String)>| -> Result<NodeOutcome> {
        let node = &plan.nodes[i];
        let section = node.section;
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
                // The requested task takes the command-line arguments; the
                // rest take the arguments their dependent called them with.
                args: if node.requested {
                    opts.args
                } else {
                    &node.args
                },
                lang_filter: opts.lang_filter,
                extra_env,
                dependency: !node.requested,
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

    fn with_code(mut s: Section) -> Section {
        s.codes = vec![crate::runner::CodeBlock {
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
    fn unknown_calls_report_the_whole_spec() {
        let sections = vec![with_code(sec("a", &["ghost x=1"], false))];
        let err = build_plan(&sections, "a", false).unwrap_err();
        assert!(matches!(err, Error::SectionNotFound(s) if s == "ghost x=1"));
    }
}
