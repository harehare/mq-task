//! `mq-task graph`: end-to-end output for both formats.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use rstest::{fixture, rstest};
use tempfile::TempDir;

const TASKS: &str = r#"
## lint

```bash
true
```

## build

```meta
retries = 2
timeout = "30s"
```

```bash
true
```

## test

```meta
depends = ["build"]
if = 'os() == "linux"'
```

```bash
true
```

## _internal

```bash
true
```

## ci

```meta
depends = ["lint", "test", "_internal"]
parallel = true
alias = ["all"]
```
"#;

#[fixture]
fn dir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("tasks.md"), TASKS).unwrap();
    dir
}

fn graph(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .arg("graph")
        .args(args)
        .args(["-f", "tasks.md"])
        .current_dir(dir)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[rstest]
fn mermaid_for_one_task(dir: TempDir) {
    let o = graph(dir.path(), &["ci", "--format", "mermaid"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);

    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("graph TD"));
    // Root first, then dependencies in discovery order.
    assert_eq!(text.lines().nth(1), Some("  n0[\"ci [parallel]\"]"));
    assert!(
        text.contains("[\"build [retries: 2, timeout: 30s]\"]"),
        "{text}"
    );
    assert!(
        text.contains("[\"test [if: os() == #quot;linux#quot;]\"]"),
        "{text}"
    );
    assert!(text.contains("[\"_internal\"]"), "{text}");
    // lint->ci, test->ci, _internal->ci, build->test
    assert_eq!(text.matches(" --> ").count(), 4, "{text}");
    assert!(text.ends_with('\n'));
}

#[rstest]
fn mermaid_without_a_task_skips_private_roots(dir: TempDir) {
    let o = graph(dir.path(), &["--format", "mermaid"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    // `_internal` is reachable through `ci`, so it is drawn exactly once.
    assert_eq!(text.matches("[\"_internal\"]").count(), 1, "{text}");
    for title in ["lint", "build", "test", "ci"] {
        assert!(
            text.contains(&format!("[\"{title}")),
            "missing {title}: {text}"
        );
    }
}

#[rstest]
#[case::lowercase("mermaid")]
#[case::uppercase("MERMAID")]
fn format_is_case_insensitive(dir: TempDir, #[case] format: &str) {
    let o = graph(dir.path(), &["ci", "--format", format]);
    assert!(out(&o).starts_with("graph TD\n"), "{}", err(&o));
}

#[rstest]
#[case::default(&[])]
#[case::text(&["--format", "text"])]
#[case::tree(&["--format", "tree"])]
fn text_is_the_default_format(dir: TempDir, #[case] extra: &[&str]) {
    let mut args = vec!["ci"];
    args.extend_from_slice(extra);
    let o = graph(dir.path(), &args);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        out(&o),
        "ci [parallel]\n├── lint\n├── test [if: os() == \"linux\"]\n│   └── build [retries: 2, timeout: 30s]\n└── _internal\n"
    );
}

#[rstest]
fn aliases_are_accepted_as_the_task(dir: TempDir) {
    let by_alias = graph(dir.path(), &["all", "--format", "mermaid"]);
    let by_title = graph(dir.path(), &["ci", "--format", "mermaid"]);
    assert!(by_alias.status.success(), "{}", err(&by_alias));
    assert_eq!(out(&by_alias), out(&by_title));
}

#[rstest]
#[case::unknown_format(&["ci", "--format", "dot"], "unknown graph format")]
#[case::unknown_task(&["nope", "--format", "mermaid"], "Section not found: nope")]
fn failures_exit_non_zero_with_a_message(
    dir: TempDir,
    #[case] args: &[&str],
    #[case] needle: &str,
) {
    let o = graph(dir.path(), args);
    assert!(!o.status.success());
    assert!(err(&o).contains(needle), "{}", err(&o));
    assert!(out(&o).is_empty(), "{}", out(&o));
}

#[test]
fn cycles_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("tasks.md"),
        "## a\n\n```meta\ndepends = [\"b\"]\n```\n\n## b\n\n```meta\ndepends = [\"a\"]\n```\n",
    )
    .unwrap();
    let o = graph(dir.path(), &["a", "--format", "mermaid"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("Circular dependency"), "{}", err(&o));
}

#[test]
fn missing_task_files_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let o = graph(dir.path(), &["--format", "mermaid"]);
    assert!(!o.status.success());
    assert!(out(&o).is_empty());
}

#[test]
fn imported_tasks_appear_with_their_namespace() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("lib.md"),
        "## compile\n\n```bash\ntrue\n```\n\n## package\n\n```meta\ndepends = [\"compile\"]\n```\n\n```bash\ntrue\n```\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("tasks.md"),
        "```meta\nimports = [\"lib.md\"]\n```\n\n## release\n\n```meta\ndepends = [\"lib:package\"]\n```\n\n```bash\ntrue\n```\n",
    )
    .unwrap();
    let o = graph(dir.path(), &["release", "--format", "mermaid"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("[\"lib:package\"]"), "{text}");
    assert!(text.contains("[\"lib:compile\"]"), "{text}");
    assert_eq!(text.matches(" --> ").count(), 2, "{text}");
}

/// Quoted titles must not break out of the Mermaid node label.
#[test]
fn quotes_in_titles_are_escaped() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("tasks.md"),
        "## say \"hi\"\n\n```bash\ntrue\n```\n",
    )
    .unwrap();
    let o = graph(dir.path(), &["say \"hi\"", "--format", "mermaid"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "graph TD\n  n0[\"say #quot;hi#quot;\"]\n");
}
