//! Dependency graph rendering (`mq-task graph`).

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::runner::Section;
use crate::schedule::find_section;

/// Output format for [`render`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFormat {
    Text,
    Mermaid,
}

impl std::str::FromStr for GraphFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "text" | "tree" => Ok(GraphFormat::Text),
            "mermaid" => Ok(GraphFormat::Mermaid),
            other => Err(format!("unknown graph format '{}' (text, mermaid)", other)),
        }
    }
}

fn badges(s: &Section) -> String {
    let mut b = Vec::new();
    if s.parallel {
        b.push("parallel".to_string());
    }
    if let Some(c) = &s.condition {
        b.push(format!("if: {}", c));
    }
    if s.retries > 0 {
        b.push(format!("retries: {}", s.retries));
    }
    if let Some(t) = s.timeout {
        b.push(format!("timeout: {}s", t.as_secs_f64()));
    }
    if b.is_empty() {
        String::new()
    } else {
        format!(" [{}]", b.join(", "))
    }
}

/// Render the dependency graph of `root`, or of every (public) task.
pub fn render(sections: &[Section], root: Option<&str>, format: GraphFormat) -> String {
    let roots: Vec<&Section> = match root {
        Some(name) => find_section(sections, name).into_iter().collect(),
        None => sections.iter().filter(|s| !s.private).collect(),
    };
    match format {
        GraphFormat::Text => text(sections, &roots),
        GraphFormat::Mermaid => mermaid(sections, &roots),
    }
}

fn text(sections: &[Section], roots: &[&Section]) -> String {
    fn walk(
        sections: &[Section],
        node: &Section,
        prefix: &str,
        out: &mut String,
        path: &mut Vec<String>,
    ) {
        let deps = &node.depends;
        for (i, dep) in deps.iter().enumerate() {
            let last = i + 1 == deps.len();
            let (branch, next_prefix) = if last {
                ("└── ", format!("{}    ", prefix))
            } else {
                ("├── ", format!("{}│   ", prefix))
            };
            match find_section(sections, dep) {
                None => {
                    let _ = writeln!(out, "{}{}{} (not found)", prefix, branch, dep);
                }
                Some(s) if path.contains(&s.title) => {
                    let _ = writeln!(out, "{}{}{} (cycle)", prefix, branch, s.title);
                }
                Some(s) => {
                    let _ = writeln!(out, "{}{}{}{}", prefix, branch, s.title, badges(s));
                    path.push(s.title.clone());
                    walk(sections, s, &next_prefix, out, path);
                    path.pop();
                }
            }
        }
    }

    let mut out = String::new();
    for root in roots {
        let _ = writeln!(out, "{}{}", root.title, badges(root));
        let mut path = vec![root.title.clone()];
        walk(sections, root, "", &mut out, &mut path);
    }
    out
}

fn mermaid(sections: &[Section], roots: &[&Section]) -> String {
    let mut ids: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<&Section> = Vec::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut missing: Vec<String> = Vec::new();

    fn id_of<'a>(
        s: &'a Section,
        ids: &mut HashMap<String, usize>,
        order: &mut Vec<&'a Section>,
    ) -> usize {
        *ids.entry(s.title.clone()).or_insert_with(|| {
            order.push(s);
            order.len() - 1
        })
    }

    let mut stack: Vec<&Section> = roots.to_vec();
    while let Some(s) = stack.pop() {
        let from = id_of(s, &mut ids, &mut order);
        if !seen.insert(s.title.clone()) {
            continue;
        }
        for dep in &s.depends {
            match find_section(sections, dep) {
                Some(d) => {
                    let to = id_of(d, &mut ids, &mut order);
                    edges.push((from, to));
                    stack.push(d);
                }
                None => missing.push(dep.clone()),
            }
        }
    }
    edges.sort_unstable();
    edges.dedup();

    let esc = |s: &str| s.replace('"', "#quot;");
    let mut out = String::from("graph TD\n");
    for (i, s) in order.iter().enumerate() {
        let _ = writeln!(out, "  n{}[\"{}{}\"]", i, esc(&s.title), esc(&badges(s)));
    }
    for (from, to) in edges {
        // Arrow points from prerequisite to dependent: "runs before".
        let _ = writeln!(out, "  n{} --> n{}", to, from);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(title: &str, depends: &[&str]) -> Section {
        Section {
            title: title.into(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn renders_tree() {
        let sections = vec![
            sec("lint", &[]),
            sec("build", &[]),
            sec("test", &["build"]),
            sec("ci", &["lint", "test"]),
        ];
        let out = render(&sections, Some("ci"), GraphFormat::Text);
        assert_eq!(out, "ci\n├── lint\n└── test\n    └── build\n");
    }

    #[test]
    fn renders_mermaid() {
        let sections = vec![sec("a", &[]), sec("b", &["a"])];
        let out = render(&sections, Some("b"), GraphFormat::Mermaid);
        assert!(out.starts_with("graph TD\n"));
        assert!(out.contains("[\"b\"]"));
        assert!(out.contains("-->"));
    }

    #[test]
    fn marks_missing_and_cycles() {
        let sections = vec![sec("a", &["b", "ghost"]), sec("b", &["a"])];
        let out = render(&sections, Some("a"), GraphFormat::Text);
        assert!(out.contains("b\n"));
        assert!(out.contains("a (cycle)"));
        assert!(out.contains("ghost (not found)"));
    }
}
