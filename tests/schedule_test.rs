//! Plan construction (`build_plan`) and execution (`run_plan`).

#[path = "common/helpers.rs"]
mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::AtomicBool;

use common::{Recorder, bash, sec};
use mq_task::config::Config;
use mq_task::error::Error;
use mq_task::events::TaskStatus;
use mq_task::exec::Executor;
use mq_task::runner::Section;
use mq_task::schedule::{Plan, ScheduleOptions, build_plan, find_section, run_plan};
use proptest::prelude::*;
use rstest::rstest;

fn index_of(plan: &Plan, title: &str) -> usize {
    plan.nodes
        .iter()
        .position(|n| n.section.title == title)
        .unwrap_or_else(|| panic!("{title} not in plan"))
}

fn par(mut s: Section) -> Section {
    s.parallel = true;
    s
}

#[rstest]
#[case("build", Some("build"))]
#[case("b", Some("build"))]
#[case("compile", Some("build"))]
#[case("test", Some("test"))]
#[case("Build", None)]
#[case("", None)]
#[case("missing", None)]
fn find_section_matches_titles_and_aliases(#[case] name: &str, #[case] expected: Option<&str>) {
    let mut build = sec("build", &[]);
    build.aliases = vec!["b".into(), "compile".into()];
    let sections = vec![build, sec("test", &[])];
    assert_eq!(
        find_section(&sections, name).map(|s| s.title.as_str()),
        expected
    );
}

#[test]
fn find_section_prefers_the_first_match() {
    let mut first = sec("a", &[]);
    first.aliases = vec!["x".into()];
    let sections = vec![first, sec("x", &[])];
    assert_eq!(find_section(&sections, "x").unwrap().title, "a");
}

#[rstest]
#[case::unknown_target(vec![bash("a", &[], "true")], "zzz", "Section not found: zzz")]
#[case::unknown_dependency(vec![bash("a", &["ghost"], "true")], "a", "Section not found: ghost")]
#[case::self_cycle(vec![bash("a", &["a"], "true")], "a", "Circular dependency")]
#[case::two_cycle(vec![sec("a", &["b"]), sec("b", &["a"])], "a", "Circular dependency")]
#[case::three_cycle(
    vec![sec("a", &["b"]), sec("b", &["c"]), sec("c", &["a"])],
    "a",
    "Circular dependency"
)]
#[case::nothing_to_run(vec![sec("a", &[])], "a", "has no commands to run")]
fn build_plan_errors(#[case] sections: Vec<Section>, #[case] target: &str, #[case] msg: &str) {
    let err = build_plan(&sections, target, false).unwrap_err();
    assert!(err.to_string().contains(msg), "{err}");
}

#[test]
fn build_plan_error_variants() {
    let sections = vec![sec("a", &["b"]), sec("b", &["a"]), sec("empty", &[])];
    assert!(matches!(
        build_plan(&sections, "a", false),
        Err(Error::CircularDependency(_))
    ));
    assert!(matches!(
        build_plan(&sections, "nope", false),
        Err(Error::SectionNotFound(n)) if n == "nope"
    ));
    assert!(matches!(
        build_plan(&sections, "empty", false),
        Err(Error::NoCommands(n)) if n == "empty"
    ));
}

#[test]
fn a_target_with_only_dependencies_is_allowed() {
    // Aggregate tasks have no code of their own.
    let sections = vec![bash("a", &[], "true"), sec("all", &["a"])];
    let plan = build_plan(&sections, "all", false).unwrap();
    assert_eq!(plan.names(), vec!["a", "all"]);
}

#[test]
fn the_target_may_be_requested_by_alias() {
    let mut a = bash("build", &[], "true");
    a.aliases = vec!["b".into()];
    let sections = [a];
    let plan = build_plan(&sections, "b", false).unwrap();
    assert_eq!(plan.names(), vec!["build"]);
}

#[test]
fn the_same_task_by_title_and_alias_is_planned_once() {
    let mut base = bash("base", &[], "true");
    base.aliases = vec!["b".into()];
    let sections = vec![base, sec("top", &["base", "b"])];
    let plan = build_plan(&sections, "top", false).unwrap();
    assert_eq!(plan.names(), vec!["base", "top"]);
    assert_eq!(plan.nodes[1].deps, vec![0, 0]);
}

#[rstest]
#[case::sequential(false, false, true)]
#[case::task_parallel(true, false, false)]
#[case::forced_parallel(false, true, false)]
#[case::both(true, true, false)]
fn sibling_dependencies_are_ordered_only_when_sequential(
    #[case] task_parallel: bool,
    #[case] force: bool,
    #[case] ordered: bool,
) {
    let mut ci = sec("ci", &["a", "b", "c"]);
    ci.parallel = task_parallel;
    let sections = vec![
        bash("a", &[], "true"),
        bash("b", &[], "true"),
        bash("c", &[], "true"),
        ci,
    ];
    let plan = build_plan(&sections, "ci", force).unwrap();

    let (a, b, c) = (
        index_of(&plan, "a"),
        index_of(&plan, "b"),
        index_of(&plan, "c"),
    );
    assert!(plan.nodes[a].after.is_empty());
    assert_eq!(plan.nodes[b].after.contains(&a), ordered);
    assert_eq!(plan.nodes[c].after.contains(&b), ordered);
    // The target always waits for all of its declared dependencies.
    let root = plan.root();
    assert_eq!(plan.nodes[root].after, BTreeSet::from([a, b, c]));
    assert_eq!(plan.nodes[root].deps, vec![a, b, c]);
}

#[test]
fn a_parallel_group_nested_in_a_sequential_task_stays_parallel_inside() {
    // outer (sequential): [pre, group]; group (parallel): [x, y].
    let sections = vec![
        bash("pre", &[], "true"),
        bash("x", &[], "true"),
        bash("y", &[], "true"),
        par(sec("group", &["x", "y"])),
        sec("outer", &["pre", "group"]),
    ];
    let plan = build_plan(&sections, "outer", false).unwrap();
    let (pre, x, y) = (
        index_of(&plan, "pre"),
        index_of(&plan, "x"),
        index_of(&plan, "y"),
    );
    // x and y are pulled in by `group`, which comes after `pre`...
    assert!(plan.nodes[x].after.contains(&pre));
    assert!(plan.nodes[y].after.contains(&pre));
    // ...but they are not ordered against each other.
    assert!(!plan.nodes[y].after.contains(&x));
    assert!(!plan.nodes[x].after.contains(&y));
}

#[test]
fn already_planned_dependencies_get_no_extra_ordering() {
    // `shared` is visited under `first`; it must not be forced after `second`.
    let sections = vec![
        bash("shared", &[], "true"),
        bash("first", &["shared"], "true"),
        bash("second", &[], "true"),
        sec("all", &["first", "second", "shared"]),
    ];
    let plan = build_plan(&sections, "all", false).unwrap();
    let shared = index_of(&plan, "shared");
    let second = index_of(&plan, "second");
    assert!(!plan.nodes[shared].after.contains(&second));
}

/// `deps[i]` only ever refers to smaller indices, so the graph is acyclic.
fn dag() -> impl Strategy<Value = (Vec<Vec<usize>>, Vec<bool>, usize)> {
    (1usize..10).prop_flat_map(|n| {
        (
            prop::collection::vec(prop::collection::vec(any::<bool>(), 10), n),
            prop::collection::vec(any::<bool>(), n),
            0..n,
        )
            .prop_map(move |(bits, parallel, root)| {
                let deps = (0..n)
                    .map(|i| (0..i).filter(|&j| bits[i][j]).collect())
                    .collect();
                (deps, parallel, root)
            })
    })
}

fn to_sections(deps: &[Vec<usize>], parallel: &[bool]) -> Vec<Section> {
    deps.iter()
        .enumerate()
        .map(|(i, ds)| {
            let names: Vec<String> = ds.iter().map(|d| format!("t{d}")).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut s = bash(&format!("t{i}"), &refs, "true");
            s.parallel = parallel[i];
            s
        })
        .collect()
}

fn reachable(deps: &[Vec<usize>], from: usize) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(i) = stack.pop() {
        if seen.insert(i) {
            stack.extend(deps[i].iter().copied());
        }
    }
    seen
}

proptest! {
    #[test]
    fn plans_are_topologically_ordered_and_minimal(
        (deps, parallel, root) in dag(),
        force in any::<bool>(),
    ) {
        let sections = to_sections(&deps, &parallel);
        let target = format!("t{root}");
        let plan = build_plan(&sections, &target, force).unwrap();

        // Exactly the reachable tasks, each once, with the target last.
        let names: Vec<String> = plan.names();
        let unique: BTreeSet<&String> = names.iter().collect();
        prop_assert_eq!(unique.len(), names.len());
        let expected: BTreeSet<String> =
            reachable(&deps, root).iter().map(|i| format!("t{i}")).collect();
        prop_assert_eq!(names.iter().cloned().collect::<BTreeSet<_>>(), expected);
        prop_assert_eq!(&names[plan.root()], &target);

        let pos: HashMap<&str, usize> =
            names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
        for (i, node) in plan.nodes.iter().enumerate() {
            // Declared dependencies match the section and precede the node.
            let declared: Vec<usize> = sections[node.section.title[1..].parse::<usize>().unwrap()]
                .depends
                .iter()
                .map(|d| pos[d.as_str()])
                .collect();
            prop_assert_eq!(&node.deps, &declared);
            // `after` is a superset of the declared deps and never points
            // forward, so the plan is always schedulable.
            for d in &node.deps {
                prop_assert!(node.after.contains(d));
            }
            for &a in &node.after {
                prop_assert!(a < i, "node {} waits for later node {}", i, a);
            }
        }
    }

    #[test]
    fn forced_parallel_adds_no_ordering_beyond_declared_deps(
        (deps, parallel, root) in dag(),
    ) {
        let sections = to_sections(&deps, &parallel);
        let plan = build_plan(&sections, &format!("t{root}"), true).unwrap();
        for node in &plan.nodes {
            let declared: BTreeSet<usize> = node.deps.iter().copied().collect();
            prop_assert_eq!(&node.after, &declared);
        }
    }

    #[test]
    fn sequential_plans_only_add_edges(
        (deps, parallel, root) in dag(),
    ) {
        // Forcing parallel can only remove ordering constraints.
        let sections = to_sections(&deps, &parallel);
        let target = format!("t{root}");
        let seq = build_plan(&sections, &target, false).unwrap();
        let par = build_plan(&sections, &target, true).unwrap();
        prop_assert_eq!(seq.names(), par.names());
        for (s, p) in seq.nodes.iter().zip(&par.nodes) {
            prop_assert!(p.after.is_subset(&s.after));
        }
    }

    #[test]
    fn any_back_edge_is_reported_as_a_cycle(n in 2usize..8, back in 0usize..8) {
        // A chain t0 <- t1 <- ... <- t(n-1), plus t0 depending on some later task.
        let back = 1 + back % (n - 1);
        let sections: Vec<Section> = (0..n)
            .map(|i| {
                let mut ds = Vec::new();
                if i > 0 { ds.push(format!("t{}", i - 1)); }
                if i == 0 { ds.push(format!("t{back}")); }
                let refs: Vec<&str> = ds.iter().map(String::as_str).collect();
                bash(&format!("t{i}"), &refs, "true")
            })
            .collect();
        let result = build_plan(&sections, &format!("t{}", n - 1), false);
        let is_cycle = matches!(result, Err(Error::CircularDependency(_)));
        prop_assert!(is_cycle);
    }
}

struct Run {
    result: Result<(), Error>,
    rec: Recorder,
}

fn run(sections: &[Section], target: &str, jobs: usize, keep_going: bool) -> Run {
    run_with(
        sections,
        target,
        jobs,
        keep_going,
        &AtomicBool::new(false),
        &[],
    )
}

fn run_with(
    sections: &[Section],
    target: &str,
    jobs: usize,
    keep_going: bool,
    cancel: &AtomicBool,
    args: &[String],
) -> Run {
    let mut exec = Executor::new(Config::default());
    exec.use_cache = false;
    let plan = build_plan(sections, target, jobs > 1).unwrap();
    let rec = Recorder::default();
    let result = run_plan(
        &exec,
        &plan,
        &ScheduleOptions {
            args,
            lang_filter: None,
            jobs,
            keep_going,
            piped: true,
            cancel,
            observer: &rec,
        },
    );
    Run { result, rec }
}

fn succeeded(r: &Run, task: &str) -> bool {
    r.rec
        .statuses(task)
        .iter()
        .any(|s| matches!(s, TaskStatus::Succeeded { .. }))
}

fn cancelled(r: &Run, task: &str) -> bool {
    r.rec
        .statuses(task)
        .iter()
        .any(|s| matches!(s, TaskStatus::Cancelled { .. }))
}

fn started(r: &Run, task: &str) -> bool {
    r.rec
        .statuses(task)
        .iter()
        .any(|s| matches!(s, TaskStatus::Started { .. }))
}

#[rstest]
#[case::sequential(1)]
#[case::parallel(4)]
fn successful_runs_respect_dependency_order(#[case] jobs: usize) {
    let sections = vec![
        bash("a", &[], "echo a"),
        bash("b", &["a"], "echo b"),
        bash("c", &["a"], "echo c"),
        bash("d", &["b", "c"], "echo d"),
    ];
    let r = run(&sections, "d", jobs, false);
    r.result.as_ref().unwrap();
    for t in ["a", "b", "c", "d"] {
        assert!(succeeded(&r, t), "{t} did not succeed");
    }
    for (before, after) in [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")] {
        let done = r
            .rec
            .position(before, |s| matches!(s, TaskStatus::Succeeded { .. }))
            .unwrap();
        let start = r
            .rec
            .position(after, |s| matches!(s, TaskStatus::Started { .. }))
            .unwrap();
        assert!(done < start, "{before} should finish before {after} starts");
    }
}

#[rstest]
#[case::sequential(1)]
#[case::parallel(3)]
fn output_is_routed_through_the_observer(#[case] jobs: usize) {
    let sections = vec![bash("a", &[], "echo hello; echo oops >&2")];
    let r = run(&sections, "a", jobs, false);
    r.result.unwrap();
    assert_eq!(r.rec.stdout("a"), vec!["hello"]);
    assert!(r.rec.events().iter().any(|e| matches!(
        e,
        common::Event::Output(t, mq_task::events::Stream::Stderr, l) if t == "a" && l == "oops"
    )));
}

#[rstest]
#[case::sequential(1)]
#[case::parallel(4)]
fn a_failure_stops_the_run_and_returns_the_exit_code(#[case] jobs: usize) {
    let sections = vec![
        bash("bad", &[], "exit 7"),
        bash("after", &["bad"], "echo should-not-run"),
        bash("root", &["after"], "echo nor-this"),
    ];
    let r = run(&sections, "root", jobs, false);
    assert!(matches!(r.result, Err(Error::ExecutionFailed(7))));
    assert!(
        r.rec
            .statuses("bad")
            .iter()
            .any(|s| matches!(s, TaskStatus::Failed { .. }))
    );
    assert!(!started(&r, "after"));
    assert!(!started(&r, "root"));
    assert!(r.rec.stdout("after").is_empty());
}

#[test]
fn parallel_fail_fast_reports_unstarted_tasks_as_cancelled() {
    let sections = vec![
        bash("bad", &[], "exit 1"),
        bash("after", &["bad"], "true"),
        bash("root", &["after"], "true"),
    ];
    let r = run(&sections, "root", 4, false);
    assert!(r.result.is_err());
    assert!(cancelled(&r, "after"));
    assert!(cancelled(&r, "root"));
}

#[rstest]
#[case::sequential(1)]
#[case::parallel(4)]
fn keep_going_runs_independent_tasks_but_not_dependents(#[case] jobs: usize) {
    // `all` is parallel, so `good` is independent of `bad`.
    let sections = vec![
        bash("bad", &[], "exit 2"),
        bash("good", &[], "echo good"),
        bash("needs_bad", &["bad"], "echo nope"),
        par(sec("all", &["bad", "good", "needs_bad"])),
    ];
    let r = run(&sections, "all", jobs, true);
    assert!(matches!(r.result, Err(Error::ExecutionFailed(2))));
    assert!(succeeded(&r, "good"));
    assert!(!started(&r, "needs_bad"));
    assert!(cancelled(&r, "needs_bad"));
    assert!(!started(&r, "all"));
}

#[test]
fn keep_going_in_sequential_mode_also_blocks_later_siblings() {
    // Characterization: without `parallel`, declaration order is an ordering
    // constraint, so a later sibling waits on (and is blocked by) a failure
    // even when it does not depend on it.
    let sections = vec![
        bash("bad", &[], "exit 2"),
        bash("good", &[], "echo good"),
        sec("all", &["bad", "good"]),
    ];
    let r = run(&sections, "all", 1, true);
    assert!(matches!(r.result, Err(Error::ExecutionFailed(2))));
    assert!(!started(&r, "good"));
    assert!(cancelled(&r, "good"));
}

#[test]
fn the_first_error_wins_with_keep_going() {
    let sections = vec![
        bash("one", &[], "exit 3"),
        bash("two", &[], "exit 4"),
        par(sec("all", &["one", "two"])),
    ];
    let r = run(&sections, "all", 1, true);
    assert!(matches!(r.result, Err(Error::ExecutionFailed(3))));
    assert!(
        r.rec
            .statuses("two")
            .iter()
            .any(|s| matches!(s, TaskStatus::Failed { .. }))
    );
}

#[test]
fn a_pre_raised_cancel_flag_runs_nothing() {
    let sections = vec![bash("a", &[], "echo a"), bash("b", &["a"], "echo b")];
    let cancel = AtomicBool::new(true);
    let r = run_with(&sections, "b", 1, false, &cancel, &[]);
    assert!(matches!(r.result, Err(Error::Cancelled)));
    assert!(!started(&r, "a"));
    assert!(r.rec.stdout("a").is_empty());
}

#[rstest]
#[case::sequential(1)]
#[case::parallel(2)]
fn skipped_tasks_do_not_block_dependents(#[case] jobs: usize) {
    let mut gated = bash("gated", &[], "echo gated");
    gated.condition = Some("false".into());
    let sections = vec![gated, bash("main", &["gated"], "echo main")];
    let r = run(&sections, "main", jobs, false);
    r.result.unwrap();
    assert!(r.rec.statuses("gated").iter().any(|s| matches!(
        s,
        TaskStatus::Skipped { reason } if reason == "if: false"
    )));
    assert_eq!(r.rec.stdout("main"), vec!["main"]);
}

#[test]
fn arguments_reach_only_the_requested_task() {
    let sections = vec![
        bash("dep", &[], "echo dep:${MX_ARGS:-none}"),
        bash("main", &["dep"], "echo main:$MX_ARGS"),
    ];
    let args = vec!["x".to_string(), "y".to_string()];
    let r = run_with(&sections, "main", 1, false, &AtomicBool::new(false), &args);
    r.result.unwrap();
    assert_eq!(r.rec.stdout("dep"), vec!["dep:none"]);
    assert_eq!(r.rec.stdout("main"), vec!["main:x y"]);
}

#[test]
fn captured_output_is_exposed_to_transitive_dependents_only() {
    let mut produce = bash("produce", &[], "echo value");
    produce.capture = true;
    let sections = vec![
        produce,
        bash("middle", &["produce"], "echo mid:$MX_OUTPUT_PRODUCE"),
        bash("top", &["middle"], "echo top:$MX_OUTPUT_PRODUCE"),
        bash("unrelated", &[], "echo un:${MX_OUTPUT_PRODUCE:-unset}"),
        sec("all", &["top", "unrelated"]),
    ];
    let r = run(&sections, "all", 1, false);
    r.result.unwrap();
    assert_eq!(r.rec.stdout("middle"), vec!["mid:value"]);
    assert_eq!(r.rec.stdout("top"), vec!["top:value"]);
    assert_eq!(r.rec.stdout("unrelated"), vec!["un:unset"]);
}

#[test]
fn parallel_runs_overlap() {
    use std::time::Instant;
    let sections = vec![
        bash("s1", &[], "sleep 0.5"),
        bash("s2", &[], "sleep 0.5"),
        bash("s3", &[], "sleep 0.5"),
        sec("all", &["s1", "s2", "s3"]),
    ];
    let t = Instant::now();
    let r = run(&sections, "all", 3, false);
    r.result.unwrap();
    assert!(t.elapsed().as_millis() < 1300, "took {:?}", t.elapsed());
}

#[test]
fn jobs_limit_caps_concurrency() {
    use std::time::Instant;
    let sections = vec![
        bash("s1", &[], "sleep 0.4"),
        bash("s2", &[], "sleep 0.4"),
        bash("s3", &[], "sleep 0.4"),
        bash("s4", &[], "sleep 0.4"),
        sec("all", &["s1", "s2", "s3", "s4"]),
    ];
    let t = Instant::now();
    let r = run(&sections, "all", 2, false);
    r.result.unwrap();
    // Four 0.4s tasks, two at a time: at least two waves.
    assert!(t.elapsed().as_millis() >= 780, "took {:?}", t.elapsed());
}
