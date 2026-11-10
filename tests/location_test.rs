//! Failures say where in the task file the failing code block is written.
#![cfg(unix)]

use std::process::{Command, Output};

use mq_task::Runner;

const TASKS: &str = "\
# Project

## ok

```bash
true
```

## build

Compile things.

```bash
echo compiling
```

```text
not run: data block
```

```bash
echo failing >&2
exit 3
```
";

fn run(tasks: &str, args: &[&str], github: bool) -> (Output, String, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("tasks.md"), tasks).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mq-task"));
    cmd.args(["-f", "tasks.md"])
        .args(args)
        .current_dir(dir.path())
        .env("NO_COLOR", "1")
        .env_remove("GITHUB_ACTIONS");
    if github {
        cmd.env("GITHUB_ACTIONS", "true");
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out, stdout, stderr)
}

#[test]
fn sections_know_their_lines() {
    let mut runner = Runner::new(mq_task::Config::default());
    let sections = runner.extract_sections(TASKS).unwrap();
    let build = sections.iter().find(|s| s.title == "build").unwrap();
    assert_eq!(build.line, Some(9));
    // The data block is not a runnable block, so two blocks remain.
    assert_eq!(build.block_lines, vec![13, 21]);
}

#[test]
fn a_failing_block_is_reported_with_its_line() {
    let (out, _, stderr) = run(TASKS, &["build"], false);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr.contains("(in task 'build', tasks.md:21)"),
        "{stderr}"
    );
}

#[test]
fn github_annotations_point_at_the_line() {
    let (_, stdout, _) = run(TASKS, &["build"], true);
    assert!(
        stdout.contains("::error file=tasks.md,line=21,title=mq-task::"),
        "{stdout}"
    );
}

#[test]
fn the_first_block_is_blamed_when_it_fails() {
    let tasks = "## t\n\n```bash\nexit 1\n```\n\n```bash\necho never\n```\n";
    let (_, _, stderr) = run(tasks, &["t"], false);
    assert!(stderr.contains("tasks.md:3"), "{stderr}");
}

#[test]
fn imported_tasks_point_into_their_own_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tasks.md"),
        "```meta\nimports = [\"lib.md\"]\n```\n\n## main\n\n```meta\ndepends = [\"lib:boom\"]\n```\n\n```bash\ntrue\n```\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib.md"),
        "## boom\n\n\n```bash\nexit 4\n```\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(["-f", "tasks.md", "main"])
        .current_dir(dir.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(4));
    assert!(stderr.contains("lib.md:4"), "{stderr}");
}

#[test]
fn a_failing_defer_is_not_blamed_on_a_body_that_recovered() {
    // The body fails once (line 12) and succeeds on retry; cleanup (line 19) fails.
    let tasks = "\
# Project

## main

```meta
retries = 1
retry_delay = \"10ms\"
defer = [\"cleanup\"]
```

```bash
if [ -f tried ]; then exit 0; fi
touch tried
exit 1
```

## cleanup

```bash
exit 2
```
";
    let (out, _, stderr) = run(tasks, &["main"], false);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("tasks.md:19"), "{stderr}");
    assert!(!stderr.contains("tasks.md:12"), "{stderr}");
}
