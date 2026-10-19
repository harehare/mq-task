//! Running several tasks, `--no-deps`, `--quiet`, `--timings`, `--status`
//! and the GitHub Actions output.

#[path = "common/helpers.rs"]
mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use common::Recorder;
use mq_task::events::TaskStatus;
use mq_task::{Config, Error, RunOptions, Runner};
use rstest::{fixture, rstest};
use tempfile::TempDir;

const TASKS: &str = r#"
## base

```bash
echo base
```

## x

```meta
depends = ["base"]
```

```bash
echo x
```

## y

```meta
depends = ["base"]
```

```bash
echo y
```

## built

```meta
sources = ["in.txt"]
outputs = ["out.txt"]
```

```bash
cp in.txt out.txt
```

## boom

```bash
exit 3
```
"#;

#[fixture]
fn dir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("tasks.md"), TASKS).unwrap();
    fs::write(dir.path().join("in.txt"), "in").unwrap();
    dir
}

fn runner() -> Runner {
    let mut r = Runner::new(Config::default());
    r.set_use_cache(false);
    r
}

fn tasks(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn started(rec: &Recorder, task: &str) -> bool {
    !rec.statuses(task).is_empty()
}

#[rstest]
fn several_tasks_run_in_order_and_share_dependencies(dir: TempDir) {
    let rec = Arc::new(Recorder::default());
    runner()
        .run_tasks_with_options(
            dir.path().join("tasks.md"),
            &tasks(&["y", "x"]),
            RunOptions {
                observer: Some(rec.clone()),
                ..Default::default()
            },
        )
        .unwrap();

    // `base` is a dependency of both and runs once, before either.
    assert_eq!(rec.stdout("base"), ["base"]);
    let base = rec.position("base", |s| matches!(s, TaskStatus::Succeeded { .. }));
    let y = rec.position("y", |s| matches!(s, TaskStatus::Started { .. }));
    let x = rec.position("x", |s| matches!(s, TaskStatus::Started { .. }));
    assert!(base < y && y < x, "{base:?} {y:?} {x:?}");
}

#[rstest]
fn arguments_are_only_for_a_single_task(dir: TempDir) {
    let err = runner()
        .run_tasks_with_options(
            dir.path().join("tasks.md"),
            &tasks(&["x", "y"]),
            RunOptions {
                args: vec!["a".into()],
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
}

#[rstest]
fn an_unknown_task_among_several_runs_nothing(dir: TempDir) {
    let rec = Arc::new(Recorder::default());
    let err = runner()
        .run_tasks_with_options(
            dir.path().join("tasks.md"),
            &tasks(&["x", "nope"]),
            RunOptions {
                observer: Some(rec.clone()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(
        matches!(err, Error::SectionNotFound(ref n) if n == "nope"),
        "{err:?}"
    );
    assert!(!started(&rec, "x"));
}

#[rstest]
fn no_deps_runs_only_the_named_tasks(dir: TempDir) {
    let rec = Arc::new(Recorder::default());
    runner()
        .run_tasks_with_options(
            dir.path().join("tasks.md"),
            &tasks(&["x", "y"]),
            RunOptions {
                no_deps: true,
                observer: Some(rec.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!started(&rec, "base"));
    assert_eq!(rec.stdout("x"), ["x"]);
    assert_eq!(rec.stdout("y"), ["y"]);
}

#[rstest]
fn a_task_with_nothing_to_compare_always_runs(dir: TempDir) {
    let states = runner()
        .task_states(
            dir.path().join("tasks.md"),
            &tasks(&["x", "y"]),
            &RunOptions::default(),
        )
        .unwrap();
    // `base` has no sources or status, so it is out of date and `x`, `y`
    // (which depend on it) are not checked. The helper task is not listed.
    let names: Vec<_> = states.iter().map(|s| s.task.as_str()).collect();
    assert_eq!(names, ["base", "x", "y"]);
    assert!(states.iter().all(|s| !s.up_to_date));
}

fn cli(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(["-f", "tasks.md"])
        .args(args)
        .env_remove("GITHUB_ACTIONS")
        .envs(envs.iter().copied())
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
fn the_cli_accepts_several_tasks(dir: TempDir) {
    let o = cli(dir.path(), &["x", "y"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.starts_with("▶ x, y\n"), "{text}");
    assert_eq!(text.matches("base\n").count(), 1, "{text}");
    for line in ["↳ x", "↳ y"] {
        assert!(text.lines().any(|l| l.contains(line)), "{text}");
    }
}

#[rstest]
fn quiet_prints_only_the_task_output(dir: TempDir) {
    let o = cli(dir.path(), &["x", "-q"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "base\nx\n");
}

#[rstest]
fn timings_list_each_task_without_the_helper(dir: TempDir) {
    let o = cli(dir.path(), &["x", "y", "--timings"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let report = err(&o);
    assert!(report.contains("Timings"), "{report}");
    assert!(report.contains("3 ran · 0 skipped · 0 failed"), "{report}");
    assert!(!report.contains("x, y"), "{report}");
}

#[rstest]
fn status_exits_one_when_a_task_is_out_of_date(dir: TempDir) {
    let o = cli(dir.path(), &["built", "--status"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(out(&o).contains("✘ built out of date"), "{}", out(&o));

    assert!(cli(dir.path(), &["built"], &[]).status.success());
    let o = cli(dir.path(), &["built", "--status"], &[]);
    assert!(o.status.success(), "{}", out(&o));
    assert!(out(&o).contains("✔ built (up to date)"), "{}", out(&o));
}

#[rstest]
fn an_undeclared_name_value_argument_is_rejected(dir: TempDir) {
    fs::write(
        dir.path().join("tasks.md"),
        "## deploy\n\n```meta\nparams = [\"env=staging\"]\n```\n\n```bash\necho \"deploy $MX_PARAM_ENV\"\n```\n",
    )
    .unwrap();
    let o = cli(dir.path(), &["deploy", "--", "enx=prod"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("Unknown parameter 'enx' for task 'deploy' (did you mean `env`?)"),
        "{}",
        err(&o)
    );
    let o = cli(dir.path(), &["deploy", "--", "env=prod"], &[]);
    assert!(out(&o).contains("deploy prod"), "{}", out(&o));
}

#[rstest]
fn github_actions_get_groups_and_an_error_annotation(dir: TempDir) {
    let env = [("GITHUB_ACTIONS", "true")];
    let o = cli(dir.path(), &["x", "--output", "group"], &env);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("::group::base\nbase\n::endgroup::"), "{text}");
    assert!(!text.contains('▶'), "{text}");

    let o = cli(dir.path(), &["boom"], &env);
    assert_eq!(o.status.code(), Some(3));
    assert!(
        out(&o).contains("::error title=mq-task::Task exited with status code 3"),
        "{}",
        out(&o)
    );
    // Not on GitHub: no workflow commands.
    let o = cli(dir.path(), &["boom"], &[]);
    assert!(!out(&o).contains("::error"), "{}", out(&o));
}
