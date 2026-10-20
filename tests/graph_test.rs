//! `graph::render`: text tree and Mermaid output.

#[path = "common/helpers.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use common::sec;
use mq_task::graph::{GraphFormat, render};
use mq_task::section::Section;
use proptest::prelude::*;
use rstest::rstest;

fn mermaid(sections: &[Section], root: Option<&str>) -> String {
    render(sections, root, GraphFormat::Mermaid)
}

fn text(sections: &[Section], root: Option<&str>) -> String {
    render(sections, root, GraphFormat::Text)
}

#[rstest]
#[case("text", GraphFormat::Text)]
#[case("tree", GraphFormat::Text)]
#[case("mermaid", GraphFormat::Mermaid)]
#[case("MERMAID", GraphFormat::Mermaid)]
#[case("Text", GraphFormat::Text)]
fn parses_graph_formats(#[case] raw: &str, #[case] expected: GraphFormat) {
    assert_eq!(raw.parse::<GraphFormat>().unwrap(), expected);
}

#[rstest]
#[case("")]
#[case("dot")]
#[case("mermaid ")]
#[case("json")]
fn rejects_unknown_graph_formats(#[case] raw: &str) {
    let err = raw.parse::<GraphFormat>().unwrap_err();
    assert!(err.contains("unknown graph format"), "{err}");
    assert!(err.contains("text, mermaid"), "{err}");
}

#[test]
fn mermaid_single_node() {
    let s = vec![sec("build", &[])];
    assert_eq!(mermaid(&s, Some("build")), "graph TD\n  n0[\"build\"]\n");
}

#[test]
fn mermaid_arrows_point_from_prerequisite_to_dependent() {
    let s = vec![sec("a", &[]), sec("b", &["a"])];
    // `a` runs before `b`, so the arrow goes a --> b. The root is n0.
    assert_eq!(
        mermaid(&s, Some("b")),
        "graph TD\n  n0[\"b\"]\n  n1[\"a\"]\n  n1 --> n0\n"
    );
}

#[test]
fn mermaid_diamond_lists_shared_dependency_once() {
    let s = vec![
        sec("base", &[]),
        sec("left", &["base"]),
        sec("right", &["base"]),
        sec("top", &["left", "right"]),
    ];
    let out = mermaid(&s, Some("top"));
    let nodes = out.lines().filter(|l| l.contains("[\"")).count();
    let edges = out.lines().filter(|l| l.contains("-->")).count();
    assert_eq!(nodes, 4, "{out}");
    assert_eq!(edges, 4, "{out}");
    assert_eq!(out.matches("[\"base\"]").count(), 1, "{out}");
}

#[test]
fn mermaid_duplicate_declared_dependency_yields_one_edge() {
    let s = vec![sec("a", &[]), sec("b", &["a", "a"])];
    let out = mermaid(&s, Some("b"));
    assert_eq!(out.matches("-->").count(), 1, "{out}");
}

#[test]
fn mermaid_terminates_on_cycles() {
    let s = vec![sec("a", &["b"]), sec("b", &["a"])];
    let out = mermaid(&s, Some("a"));
    assert!(out.starts_with("graph TD\n"));
    assert_eq!(out.matches("-->").count(), 2, "{out}");
}

#[test]
fn mermaid_self_dependency_terminates() {
    let s = vec![sec("a", &["a"])];
    let out = mermaid(&s, Some("a"));
    assert_eq!(out, "graph TD\n  n0[\"a\"]\n  n0 --> n0\n");
}

#[test]
fn mermaid_unknown_root_is_an_empty_graph() {
    let s = vec![sec("a", &[])];
    assert_eq!(mermaid(&s, Some("nope")), "graph TD\n");
}

#[test]
fn mermaid_empty_document_is_an_empty_graph() {
    assert_eq!(mermaid(&[], None), "graph TD\n");
}

#[test]
fn mermaid_resolves_aliases_for_root_and_dependencies() {
    let mut test = sec("test", &[]);
    test.aliases = vec!["t".into()];
    let s = vec![test, sec("ci", &["t"])];

    // Dependency written as an alias resolves to the real task's title.
    let out = mermaid(&s, Some("ci"));
    assert!(out.contains("[\"test\"]"), "{out}");
    assert!(!out.contains("[\"t\"]"), "{out}");
    assert_eq!(out.matches("-->").count(), 1);

    // And so does a root given as an alias.
    assert_eq!(mermaid(&s, Some("t")), "graph TD\n  n0[\"test\"]\n");
}

#[test]
fn mermaid_omits_missing_dependencies() {
    // Unlike the text format, a missing dependency is not drawn.
    let s = vec![sec("a", &["ghost"])];
    assert_eq!(mermaid(&s, Some("a")), "graph TD\n  n0[\"a\"]\n");
    assert!(text(&s, Some("a")).contains("ghost (not found)"));
}

#[test]
fn without_a_root_only_public_tasks_are_roots() {
    let mut hidden = sec("_hidden", &[]);
    hidden.private = true;
    let s = vec![sec("pub", &[]), hidden, sec("uses", &["_hidden"])];

    let out = mermaid(&s, None);
    // `_hidden` still shows up as a dependency of `uses`, but not as a root.
    assert!(out.contains("[\"pub\"]") && out.contains("[\"uses\"]"));
    assert_eq!(out.matches("[\"_hidden\"]").count(), 1);
    assert_eq!(out.matches("-->").count(), 1);

    let only_hidden = mermaid(&[s[1].clone()], None);
    assert_eq!(only_hidden, "graph TD\n");
}

#[test]
fn an_explicit_root_may_be_private() {
    let mut hidden = sec("_hidden", &[]);
    hidden.private = true;
    assert_eq!(
        mermaid(&[hidden], Some("_hidden")),
        "graph TD\n  n0[\"_hidden\"]\n"
    );
}

fn badge_case(f: impl FnOnce(&mut Section)) -> String {
    let mut s = sec("t", &[]);
    f(&mut s);
    mermaid(&[s], Some("t"))
}

#[rstest]
#[case::parallel(|s: &mut Section| s.parallel = true, "[\"t [parallel]\"]")]
#[case::retries(|s: &mut Section| s.retries = 3, "[\"t [retries: 3]\"]")]
#[case::no_retries(|s: &mut Section| s.retries = 0, "[\"t\"]")]
#[case::timeout_whole(|s: &mut Section| s.timeout = Some(Duration::from_secs(30)), "[\"t [timeout: 30s]\"]")]
#[case::timeout_fraction(|s: &mut Section| s.timeout = Some(Duration::from_millis(1500)), "[\"t [timeout: 1.5s]\"]")]
#[case::condition(|s: &mut Section| s.condition = Some("flag(\"CI\")".into()), "[\"t [if: flag(#quot;CI#quot;)]\"]")]
#[case::all_in_order(
    |s: &mut Section| {
        s.timeout = Some(Duration::from_secs(5));
        s.retries = 2;
        s.condition = Some("os() == \"linux\"".into());
        s.parallel = true;
    },
    "[\"t [parallel, if: os() == #quot;linux#quot;, retries: 2, timeout: 5s]\"]"
)]
fn mermaid_badges(#[case] f: fn(&mut Section), #[case] expected: &str) {
    let out = badge_case(f);
    assert_eq!(out, format!("graph TD\n  n0{}\n", expected));
}

#[rstest]
#[case("plain", "plain")]
#[case("say \"hi\"", "say #quot;hi#quot;")]
#[case("\"", "#quot;")]
#[case("a[b]", "a[b]")]
#[case("日本語", "日本語")]
fn mermaid_escapes_double_quotes_in_titles(#[case] title: &str, #[case] shown: &str) {
    let out = mermaid(&[sec(title, &[])], Some(title));
    assert_eq!(out, format!("graph TD\n  n0[\"{}\"]\n", shown));
}

#[test]
fn text_without_dependencies_is_just_the_title() {
    assert_eq!(text(&[sec("a", &[])], Some("a")), "a\n");
}

#[test]
fn text_shows_badges_on_root_and_children() {
    let mut a = sec("a", &[]);
    a.retries = 1;
    let mut root = sec("root", &["a"]);
    root.parallel = true;
    let out = text(&[a, root], Some("root"));
    assert_eq!(out, "root [parallel]\n└── a [retries: 1]\n");
}

#[test]
fn text_repeats_shared_dependencies_under_each_parent() {
    let s = vec![
        sec("base", &[]),
        sec("x", &["base"]),
        sec("y", &["base"]),
        sec("all", &["x", "y"]),
    ];
    assert_eq!(
        text(&s, Some("all")),
        "all\n├── x\n│   └── base\n└── y\n    └── base\n"
    );
}

#[test]
fn text_lists_every_public_root_when_no_task_is_given() {
    let s = vec![sec("a", &[]), sec("b", &["a"])];
    assert_eq!(text(&s, None), "a\nb\n└── a\n");
}

#[test]
fn text_unknown_root_is_empty() {
    assert_eq!(text(&[sec("a", &[])], Some("zzz")), "");
}

/// A random task graph. `deps[i]` are indices into the graph; in a DAG they
/// are always smaller than `i`.
#[derive(Debug, Clone)]
struct Graph {
    deps: Vec<Vec<usize>>,
    private: Vec<bool>,
    root: usize,
}

impl Graph {
    fn sections(&self) -> Vec<Section> {
        self.deps
            .iter()
            .enumerate()
            .map(|(i, ds)| {
                let names: Vec<String> = ds.iter().map(|d| format!("t{d}")).collect();
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let mut s = sec(&format!("t{i}"), &refs);
                s.private = self.private[i];
                s
            })
            .collect()
    }

    fn root_name(&self) -> String {
        format!("t{}", self.root)
    }

    fn reachable(&self, from: usize) -> BTreeSet<usize> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from];
        while let Some(i) = stack.pop() {
            if seen.insert(i) {
                stack.extend(self.deps[i].iter().copied());
            }
        }
        seen
    }

    /// Distinct (dependency, dependent) pairs below `from`.
    fn edges(&self, from: usize) -> BTreeSet<(usize, usize)> {
        self.reachable(from)
            .into_iter()
            .flat_map(|i| self.deps[i].iter().map(move |&d| (d, i)))
            .collect()
    }

    /// Number of lines the text tree has for `node` (acyclic graphs only).
    fn tree_lines(&self, node: usize) -> usize {
        1 + self.deps[node]
            .iter()
            .map(|&d| self.tree_lines(d))
            .sum::<usize>()
    }
}

fn dag() -> impl Strategy<Value = Graph> {
    (1usize..9).prop_flat_map(|n| {
        (
            prop::collection::vec(prop::collection::vec(any::<bool>(), 8), n),
            prop::collection::vec(any::<bool>(), n),
            0..n,
        )
            .prop_map(move |(bits, private, root)| Graph {
                deps: (0..n)
                    .map(|i| (0..i).filter(|&j| bits[i][j]).collect())
                    .collect(),
                private,
                root,
            })
    })
}

fn any_graph() -> impl Strategy<Value = Graph> {
    (1usize..6).prop_flat_map(|n| {
        (
            prop::collection::vec(prop::collection::vec(any::<bool>(), 6), n),
            prop::collection::vec(any::<bool>(), n),
            0..n,
        )
            .prop_map(move |(bits, private, root)| Graph {
                deps: (0..n)
                    .map(|i| (0..n).filter(|&j| bits[i][j]).collect())
                    .collect(),
                private,
                root,
            })
    })
}

/// Parsed Mermaid output: node id -> label, and (from, to) id pairs.
fn parse_mermaid(out: &str) -> (BTreeMap<usize, String>, Vec<(usize, usize)>) {
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("graph TD"));
    let mut nodes = BTreeMap::new();
    let mut edges = Vec::new();
    for line in lines {
        let line = line.strip_prefix("  ").expect("indented line");
        if let Some((a, b)) = line.split_once(" --> ") {
            let id = |s: &str| s.strip_prefix('n').unwrap().parse::<usize>().unwrap();
            edges.push((id(a), id(b)));
        } else {
            let (id, rest) = line.split_once('[').expect("node line");
            let label = rest.strip_prefix('"').unwrap().strip_suffix("\"]").unwrap();
            nodes.insert(id.strip_prefix('n').unwrap().parse().unwrap(), label.into());
        }
    }
    (nodes, edges)
}

proptest! {
    #[test]
    fn mermaid_contains_exactly_the_reachable_subgraph(g in dag()) {
        let out = mermaid(&g.sections(), Some(&g.root_name()));
        let (nodes, edges) = parse_mermaid(&out);

        // Same nodes: one label per reachable task.
        let labels: BTreeSet<String> = nodes.values().cloned().collect();
        let expected: BTreeSet<String> =
            g.reachable(g.root).iter().map(|i| format!("t{i}")).collect();
        prop_assert_eq!(nodes.len(), labels.len(), "duplicate node in {}", out);
        prop_assert_eq!(&labels, &expected);

        // Node ids are dense and start at the root.
        prop_assert_eq!(nodes.keys().copied().collect::<Vec<_>>(), (0..nodes.len()).collect::<Vec<_>>());
        prop_assert_eq!(&nodes[&0], &g.root_name());

        // Same edges, no duplicates, prerequisite -> dependent.
        let drawn: BTreeSet<(String, String)> = edges
            .iter()
            .map(|(a, b)| (nodes[a].clone(), nodes[b].clone()))
            .collect();
        prop_assert_eq!(drawn.len(), edges.len(), "duplicate edge in {}", out);
        let wanted: BTreeSet<(String, String)> = g
            .edges(g.root)
            .into_iter()
            .map(|(d, p)| (format!("t{d}"), format!("t{p}")))
            .collect();
        prop_assert_eq!(drawn, wanted);
    }

    #[test]
    fn mermaid_is_deterministic(g in any_graph()) {
        let s = g.sections();
        let a = mermaid(&s, Some(&g.root_name()));
        let b = mermaid(&s, Some(&g.root_name()));
        prop_assert_eq!(a, b);
    }

    #[test]
    fn mermaid_terminates_and_stays_within_declared_edges_on_any_graph(g in any_graph()) {
        let out = mermaid(&g.sections(), Some(&g.root_name()));
        let (nodes, edges) = parse_mermaid(&out);
        let declared = g.edges(g.root);
        for (a, b) in edges {
            let da: usize = nodes[&a].trim_start_matches('t').parse().unwrap();
            let db: usize = nodes[&b].trim_start_matches('t').parse().unwrap();
            prop_assert!(declared.contains(&(da, db)), "{} -> {} not declared", da, db);
        }
        prop_assert_eq!(nodes.len(), g.reachable(g.root).len());
    }

    #[test]
    fn mermaid_without_root_starts_from_public_tasks_only(g in dag()) {
        let out = mermaid(&g.sections(), None);
        let (nodes, _) = parse_mermaid(&out);
        let mut expected = BTreeSet::new();
        for i in (0..g.deps.len()).filter(|&i| !g.private[i]) {
            expected.extend(g.reachable(i));
        }
        let labels: BTreeSet<String> = nodes.values().cloned().collect();
        let wanted: BTreeSet<String> = expected.iter().map(|i| format!("t{i}")).collect();
        prop_assert_eq!(labels, wanted);
    }

    #[test]
    fn text_tree_has_one_line_per_path(g in dag()) {
        let out = text(&g.sections(), Some(&g.root_name()));
        prop_assert_eq!(out.lines().count(), g.tree_lines(g.root));
        prop_assert_eq!(out.lines().next().unwrap(), g.root_name());
        prop_assert!(!out.contains("(cycle)"));
        prop_assert!(!out.contains("(not found)"));
    }

    #[test]
    fn text_tree_terminates_on_any_graph(g in any_graph()) {
        let out = text(&g.sections(), Some(&g.root_name()));
        prop_assert!(out.starts_with(&g.root_name()));
        // Every line either names a task or flags a cycle; nothing is "not found".
        prop_assert!(!out.contains("(not found)"));
    }

    #[test]
    fn arbitrary_titles_never_break_mermaid_node_lines(title in "[ -~]{1,20}") {
        // Printable ASCII, quotes included.
        let out = mermaid(&[sec(&title, &[])], Some(&title));
        let line = out.lines().nth(1).unwrap();
        prop_assert!(line.starts_with("  n0[\""));
        prop_assert!(line.ends_with("\"]"));
        // Only the delimiters may be raw quotes.
        let inner = &line["  n0[\"".len()..line.len() - 2];
        prop_assert!(!inner.contains('"'));
    }
}
