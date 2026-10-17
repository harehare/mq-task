//! End-to-end tests that drive the compiled `mq-task` binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

fn workdir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mq-task-e2e-{}-{}-{}",
        std::process::id(),
        name,
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn mq_task(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn dependencies_run_sequentially_by_default() {
    let dir = workdir("seq");
    write(
        &dir,
        "tasks.md",
        r#"
## a

```bash
echo A
```

## b

```bash
echo B
```

## ci

```meta
depends = ["a", "b"]
```

```bash
echo CI
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "ci"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let a = text.find('A').unwrap();
    let b = text.find("\nB").unwrap();
    let ci = text.find("CI").unwrap();
    assert!(a < b && b < ci, "{}", text);
}

#[test]
fn parallel_dependencies_overlap_and_join() {
    let dir = workdir("par");
    write(
        &dir,
        "tasks.md",
        r#"
## slow1

```bash
sleep 1
echo one-done
```

## slow2

```bash
sleep 1
echo two-done
```

## slow3

```bash
sleep 1
echo three-done
```

## all

```meta
depends = ["slow1", "slow2", "slow3"]
parallel = true
```

```bash
echo joined
```
"#,
    );
    let start = Instant::now();
    let out = mq_task(&dir, &["-f", "tasks.md", "all"]);
    let elapsed = start.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        elapsed.as_secs_f64() < 2.5,
        "expected overlap, took {:?}",
        elapsed
    );
    let text = stdout(&out);
    assert!(
        text.contains("slow1") && text.contains("one-done"),
        "{}",
        text
    );
    // The joining task runs only after every dependency finished.
    let joined = text.find("joined").unwrap();
    for done in ["one-done", "two-done", "three-done"] {
        assert!(text.find(done).unwrap() < joined, "{}", text);
    }
}

#[test]
fn jobs_one_forces_sequential() {
    let dir = workdir("jobs1");
    write(
        &dir,
        "tasks.md",
        r#"
## a

```bash
sleep 0.6
```

## b

```bash
sleep 0.6
```

## all

```meta
depends = ["a", "b"]
parallel = true
```

```bash
true
```
"#,
    );
    let start = Instant::now();
    let out = mq_task(&dir, &["-f", "tasks.md", "-j", "1", "all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(start.elapsed().as_secs_f64() >= 1.1);
}

#[test]
fn parallel_flag_overrides_task_setting() {
    let dir = workdir("parflag");
    write(
        &dir,
        "tasks.md",
        r#"
## a

```bash
sleep 1
```

## b

```bash
sleep 1
```

## all

```meta
depends = ["a", "b"]
```

```bash
true
```
"#,
    );
    let start = Instant::now();
    let out = mq_task(&dir, &["-f", "tasks.md", "--parallel", "all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(start.elapsed().as_secs_f64() < 1.8);
}

#[test]
fn failing_dependency_aborts_parallel_run() {
    let dir = workdir("failfast");
    write(
        &dir,
        "tasks.md",
        r#"
## bad

```bash
exit 3
```

## slow

```bash
sleep 5
echo should-not-print
```

## all

```meta
depends = ["bad", "slow"]
parallel = true
```

```bash
echo should-not-run
```
"#,
    );
    let start = Instant::now();
    let out = mq_task(&dir, &["-f", "tasks.md", "all"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        start.elapsed().as_secs_f64() < 3.0,
        "slow task was not killed"
    );
    let text = stdout(&out);
    assert!(!text.contains("should-not-print"));
    assert!(!text.contains("should-not-run"));
}

#[test]
fn keep_going_runs_independent_tasks_but_not_dependents() {
    let dir = workdir("keepgoing");
    write(
        &dir,
        "tasks.md",
        r#"
## bad

```bash
exit 2
```

## good

```bash
echo good-ran
```

## after-bad

```meta
depends = ["bad"]
```

```bash
echo after-bad-ran
```

## all

```meta
depends = ["after-bad", "good"]
parallel = true
```

```bash
echo all-ran
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "-k", "all"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("good-ran"), "{}", text);
    assert!(!text.contains("after-bad-ran"));
    assert!(!text.contains("all-ran"));
}

#[test]
fn if_condition_skips_and_runs() {
    let dir = workdir("if");
    write(
        &dir,
        "tasks.md",
        r#"
## gated

```meta
if = 'getenv("GO") == "yes"'
```

```bash
echo gated-ran
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "gated"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("gated-ran"));
    assert!(stdout(&out).contains("skipped"));

    let out = mq_task(&dir, &["-f", "tasks.md", "gated", "--env", "GO=yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("gated-ran"));
}

#[test]
fn skipped_dependency_does_not_block_dependents() {
    let dir = workdir("ifdep");
    write(
        &dir,
        "tasks.md",
        r#"
## maybe

```meta
if = "false"
```

```bash
echo maybe-ran
```

## main

```meta
depends = ["maybe"]
```

```bash
echo main-ran
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "main"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("main-ran") && !text.contains("maybe-ran"));
}

#[test]
fn retries_until_success() {
    let dir = workdir("retry");
    write(
        &dir,
        "tasks.md",
        r#"
## flaky

```meta
retries = 3
retry_delay = "50ms"
```

```bash
n=$(cat counter 2>/dev/null || echo 0)
n=$((n + 1))
echo $n > counter
echo "attempt $n"
[ "$n" -ge 3 ]
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "flaky"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("attempt 3"));
    assert!(stderr(&out).contains("retrying"));
}

#[test]
fn retries_exhausted_reports_failure() {
    let dir = workdir("retryfail");
    write(
        &dir,
        "tasks.md",
        r#"
## always-bad

```meta
retries = 1
retry_delay = "10ms"
```

```bash
exit 7
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "always-bad"]);
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn timeout_kills_the_task() {
    let dir = workdir("timeout");
    write(
        &dir,
        "tasks.md",
        r#"
## hang

```meta
timeout = "300ms"
```

```bash
sleep 10
```
"#,
    );
    let start = Instant::now();
    let out = mq_task(&dir, &["-f", "tasks.md", "hang"]);
    assert_eq!(out.status.code(), Some(124), "{}", stderr(&out));
    assert!(start.elapsed().as_secs_f64() < 5.0);
    assert!(stderr(&out).contains("timed out"));
}

#[test]
fn captured_output_reaches_dependents() {
    let dir = workdir("capture");
    write(
        &dir,
        "tasks.md",
        r#"
## version

```meta
capture = true
```

```bash
echo 1.2.3
```

## release

```meta
depends = ["version"]
```

```bash
echo "releasing $MX_OUTPUT_VERSION"
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "release"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("releasing 1.2.3"), "{}", stdout(&out));
}

#[test]
fn imports_expose_namespaced_tasks() {
    let dir = workdir("import");
    fs::create_dir_all(dir.join("sub")).unwrap();
    write(
        &dir,
        "sub/ci.md",
        r#"
```meta
env = ["FROM_IMPORT=yes"]
```

# CI

## lint

```bash
echo "lint $FROM_IMPORT"
```

## check

```meta
depends = ["lint"]
```

```bash
echo check
```
"#,
    );
    write(
        &dir,
        "tasks.md",
        r#"
```meta
imports = ["sub/ci.md"]
```

# Tasks

## all

```meta
depends = ["ci:check"]
```

```bash
echo all-done
```
"#,
    );

    let out = mq_task(&dir, &["-f", "tasks.md", "all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("lint yes"), "{}", text);
    assert!(text.contains("all-done"));

    let list = mq_task(&dir, &["list", "-f", "tasks.md"]);
    let listing = stdout(&list);
    assert!(
        listing.contains("ci:lint") && listing.contains("ci:check"),
        "{}",
        listing
    );
}

#[test]
fn circular_imports_are_rejected() {
    let dir = workdir("importcycle");
    write(
        &dir,
        "a.md",
        "```meta\nimports = [\"b.md\"]\n```\n\n## a\n\n```bash\ntrue\n```\n",
    );
    write(
        &dir,
        "b.md",
        "```meta\nimports = [\"a.md\"]\n```\n\n## b\n\n```bash\ntrue\n```\n",
    );
    let out = mq_task(&dir, &["-f", "a.md", "a"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("circular import"), "{}", stderr(&out));
}

#[test]
fn sources_skip_unchanged_tasks() {
    let dir = workdir("cache");
    write(&dir, "input.txt", "v1");
    write(
        &dir,
        "tasks.md",
        r#"
## build

```meta
sources = ["input.txt"]
```

```bash
echo building
echo x >> runs.log
```
"#,
    );
    let first = mq_task(&dir, &["-f", "tasks.md", "build"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let second = mq_task(&dir, &["-f", "tasks.md", "build"]);
    assert!(second.status.success());
    assert!(
        stdout(&second).contains("up to date"),
        "{}",
        stdout(&second)
    );
    assert_eq!(
        fs::read_to_string(dir.join("runs.log"))
            .unwrap()
            .lines()
            .count(),
        1
    );

    // Changing an input invalidates the cache.
    write(&dir, "input.txt", "v2");
    let third = mq_task(&dir, &["-f", "tasks.md", "build"]);
    assert!(stdout(&third).contains("building"));
    assert_eq!(
        fs::read_to_string(dir.join("runs.log"))
            .unwrap()
            .lines()
            .count(),
        2
    );

    // --no-cache always runs.
    let fourth = mq_task(&dir, &["-f", "tasks.md", "--no-cache", "build"]);
    assert!(stdout(&fourth).contains("building"));
}

#[test]
fn graph_prints_tree_and_mermaid() {
    let dir = workdir("graph");
    write(
        &dir,
        "tasks.md",
        r#"
## lint

```bash
true
```

## test

```bash
true
```

## ci

```meta
depends = ["lint", "test"]
parallel = true
```

```bash
true
```
"#,
    );
    let out = mq_task(&dir, &["graph", "ci", "-f", "tasks.md"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("ci [parallel]"), "{}", text);
    assert!(text.contains("├── lint") && text.contains("└── test"));

    let out = mq_task(
        &dir,
        &["graph", "ci", "-f", "tasks.md", "--format", "mermaid"],
    );
    assert!(stdout(&out).starts_with("graph TD"));
}

#[test]
fn invalid_meta_is_reported() {
    let dir = workdir("badmeta");
    write(
        &dir,
        "tasks.md",
        "## t\n\n```meta\ndepends = \"notalist\"\n```\n\n```bash\ntrue\n```\n",
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "t"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("meta"), "{}", stderr(&out));
}

#[test]
fn completions_are_generated() {
    let dir = workdir("completions");
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let out = mq_task(&dir, &["completions", shell]);
        assert!(out.status.success(), "{shell}: {}", stderr(&out));
        let script = stdout(&out);
        assert!(script.contains("mq-task"), "{shell}");
        // The script asks mq-task itself, so task names can be completed.
        assert!(script.contains("COMPLETE"), "{shell}: {script}");
    }
}

/// What fish would be offered after typing `mq-task <words>`.
fn fish_candidates(dir: &Path, words: &[&str], file_env: Option<&str>) -> Vec<String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mq-task"));
    cmd.env("COMPLETE", "fish")
        .arg("--")
        .arg("mq-task")
        .args(words)
        .current_dir(dir);
    if let Some(file) = file_env {
        cmd.env("MQ_TASK_FILE", file);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).lines().map(str::to_string).collect()
}

const COMPLETION_TASKS: &str = r#"
## deploy

Ship it to production.
More detail that is not shown.

```meta
alias = ["ship"]
```

```bash
true
```

## build

```bash
true
```

## _hidden

```bash
true
```
"#;

#[test]
fn task_names_are_completed_from_the_task_file() {
    let dir = workdir("complete-tasks");
    write(&dir, "tasks.md", COMPLETION_TASKS);

    let all = fish_candidates(&dir, &[""], None);
    assert!(
        all.contains(&"deploy\tShip it to production.".to_string()),
        "{all:?}"
    );
    assert!(
        all.contains(&"ship\tShip it to production.".to_string()),
        "{all:?}"
    );
    assert!(all.contains(&"build".to_string()), "{all:?}");
    assert!(
        all.iter().any(|c| c.starts_with("run\t")),
        "subcommands too: {all:?}"
    );
    assert!(!all.iter().any(|c| c.contains("_hidden")), "{all:?}");

    assert_eq!(
        fish_candidates(&dir, &["b"], None),
        vec!["build".to_string()]
    );
    assert_eq!(
        fish_candidates(&dir, &["d"], None),
        vec!["deploy\tShip it to production.".to_string()]
    );
    assert!(fish_candidates(&dir, &["zzz"], None).is_empty());
}

#[test]
fn task_names_are_completed_after_a_subcommand_too() {
    let dir = workdir("complete-sub");
    write(&dir, "tasks.md", COMPLETION_TASKS);
    for sub in ["run", "graph"] {
        assert_eq!(
            fish_candidates(&dir, &[sub, "bu"], None),
            vec!["build".to_string()],
            "{sub}"
        );
    }
}

#[test]
fn completion_follows_mq_task_file_and_survives_a_missing_file() {
    let dir = workdir("complete-env");
    write(&dir, "other.md", "## from-other\n\n```bash\ntrue\n```\n");
    assert_eq!(
        fish_candidates(&dir, &["f"], Some("other.md")),
        vec!["from-other".to_string()]
    );
    // No task file at all: no candidates, and no error.
    assert!(fish_candidates(&dir, &["f"], None).is_empty());

    write(
        &dir,
        "broken.md",
        "```meta\nnot = [valid\n```\n## zeta\n\n```bash\ntrue\n```\n",
    );
    assert!(fish_candidates(&dir, &["z"], Some("broken.md")).is_empty());
}

#[test]
fn keep_going_runs_later_siblings_in_a_sequential_task() {
    // `all` has no `parallel`, so its dependencies are ordered. `good` still
    // must run after `bad` fails because it does not depend on it.
    let dir = workdir("keepgoing-seq");
    write(
        &dir,
        "tasks.md",
        r#"
## bad

```bash
echo bad-ran
exit 2
```

## good

```bash
echo good-ran
```

## after-bad

```meta
depends = ["bad"]
```

```bash
echo after-bad-ran
```

## all

```meta
depends = ["bad", "good", "after-bad"]
```

```bash
echo all-ran
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "-k", "all"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let text = stdout(&out);
    let bad = text.find("bad-ran").expect("bad ran");
    let good = text.find("good-ran").expect("good should still run");
    assert!(bad < good, "declaration order must be kept: {}", text);
    assert!(!text.contains("after-bad-ran"), "{}", text);
    assert!(!text.contains("all-ran"), "{}", text);
}

#[test]
fn fail_fast_reports_the_tasks_that_were_not_run() {
    let dir = workdir("failfast-report");
    write(
        &dir,
        "tasks.md",
        r#"
## bad

```bash
exit 1
```

## later

```bash
echo later-ran
```

## all

```meta
depends = ["bad", "later"]
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "all"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(!stdout(&out).contains("later-ran"));
    let err = stderr(&out);
    assert!(err.contains("later not run"), "{}", err);
    assert!(err.contains("all not run"), "{}", err);
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[test]
fn for_runs_the_task_once_per_item() {
    let dir = workdir("for-list");
    write(
        &dir,
        "tasks.md",
        r#"
## greet

```meta
for = ["alice", "bob"]
as = "who"
```

```bash
echo "hi $MX_ITEM $MX_INDEX $MX_WHO"
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "greet"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let hi: Vec<String> = lines(&text)
        .into_iter()
        .filter(|l| l.starts_with("hi "))
        .collect();
    assert_eq!(hi, vec!["hi alice 0 alice", "hi bob 1 bob"]);
    // Each iteration is introduced by its own line.
    let alice = text.find("↳ greet[alice]").expect(&text);
    let bob = text.find("↳ greet[bob]").expect(&text);
    assert!(alice < text.find("hi alice").unwrap());
    assert!(bob > text.find("hi alice").unwrap() && bob < text.find("hi bob").unwrap());
}

#[test]
fn for_matrix_and_glob_and_dependents() {
    let dir = workdir("for-matrix");
    fs::create_dir_all(dir.join("pkgs/one")).unwrap();
    fs::create_dir_all(dir.join("pkgs/two")).unwrap();
    write(
        &dir,
        "tasks.md",
        r#"
## build

```meta
for = { matrix = { os = ["linux", "mac"], arch = ["x64"] } }
```

```bash
echo "build $MX_OS-$MX_ARCH"
```

## pkgs

```meta
for = { glob = "pkgs/*" }
capture = true
```

```bash
echo "pkg $MX_ITEM"
```

## all

```meta
depends = ["build", "pkgs"]
```

```bash
echo "outputs: $MX_OUTPUT_PKGS" | tr '\n' ';'
echo
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("build linux-x64"), "{}", text);
    assert!(text.contains("build mac-x64"), "{}", text);
    assert!(text.contains("pkg pkgs/one"), "{}", text);
    assert!(text.contains("pkg pkgs/two"), "{}", text);
    assert!(
        text.contains("outputs: pkg pkgs/one;pkg pkgs/two"),
        "{}",
        text
    );
}

#[test]
fn for_loops_stop_at_the_first_failure_unless_keep_going() {
    let dir = workdir("for-fail");
    write(
        &dir,
        "tasks.md",
        r#"
## each

```meta
for = ["a", "b", "c"]
```

```bash
echo "ran $MX_ITEM"
[ "$MX_ITEM" != "b" ] || exit 3
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "each"]);
    assert_eq!(out.status.code(), Some(3));
    let text = stdout(&out);
    assert!(text.contains("ran a") && text.contains("ran b"));
    assert!(!text.contains("ran c"), "{}", text);

    let out = mq_task(&dir, &["-f", "tasks.md", "each", "--keep-going"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(stdout(&out).contains("ran c"));
}

#[test]
fn parallel_for_runs_iterations_concurrently() {
    let dir = workdir("for-par");
    write(
        &dir,
        "tasks.md",
        r#"
## nap

```meta
for = ["1", "2", "3", "4"]
parallel = true
```

```bash
sleep 0.5
```
"#,
    );
    let started = Instant::now();
    let out = mq_task(&dir, &["-f", "tasks.md", "nap", "-j", "4"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        started.elapsed().as_millis() < 1700,
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn an_empty_loop_is_skipped_not_an_error() {
    let dir = workdir("for-empty");
    write(
        &dir,
        "tasks.md",
        r#"
## none

```meta
for = { glob = "nothing/*" }
```

```bash
echo should-not-run
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "none"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("should-not-run"));
    assert!(stdout(&out).contains("no items"), "{}", stdout(&out));
}

#[test]
fn invalid_for_is_reported() {
    let dir = workdir("for-bad");
    write(
        &dir,
        "tasks.md",
        "## t\n\n```meta\nfor = \"nope\"\n```\n\n```bash\ntrue\n```\n",
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "t"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("`for`"), "{}", stderr(&out));
}

#[test]
fn dependencies_can_be_called_with_arguments() {
    let dir = workdir("dep-args");
    write(
        &dir,
        "tasks.md",
        r#"
## deploy

```meta
params = ["env", "region=us"]
```

```bash
echo "deploy $MX_PARAM_ENV $MX_PARAM_REGION"
```

## all

```meta
depends = ["deploy staging", "deploy env=prod 'region=eu west'"]
```

```bash
echo done
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let a = text.find("deploy staging us").expect(&text);
    let b = text.find("deploy prod eu west").expect(&text);
    assert!(a < b);
}

#[test]
fn defer_runs_after_success_and_failure() {
    let dir = workdir("defer");
    write(
        &dir,
        "tasks.md",
        r#"
## cleanup

```meta
params = ["note=done"]
```

```bash
echo "cleanup $MX_PARAM_NOTE"
```

## ok

```meta
defer = ["cleanup after-ok"]
```

```bash
echo body-ok
```

## boom

```meta
defer = ["cleanup after-boom"]
```

```bash
echo body-boom
exit 7
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "ok"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.find("body-ok").unwrap() < text.find("cleanup after-ok").unwrap());

    let out = mq_task(&dir, &["-f", "tasks.md", "boom"]);
    assert_eq!(out.status.code(), Some(7));
    let text = stdout(&out);
    assert!(text.find("body-boom").unwrap() < text.find("cleanup after-boom").unwrap());
}

#[test]
fn a_failing_defer_fails_a_successful_task() {
    let dir = workdir("defer-fail");
    write(
        &dir,
        "tasks.md",
        r#"
## cleanup

```bash
exit 9
```

## main

```meta
defer = ["cleanup"]
```

```bash
echo body
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "main"]);
    assert_eq!(out.status.code(), Some(9));
}

#[test]
fn each_loop_iteration_is_cached_separately() {
    let dir = workdir("for-cache");
    fs::write(dir.join("in.txt"), "x").unwrap();
    write(
        &dir,
        "tasks.md",
        r#"
## gen

```meta
for = ["a", "b"]
sources = ["in.txt"]
```

```bash
echo "gen $MX_ITEM"
```
"#,
    );
    let first = mq_task(&dir, &["-f", "tasks.md", "gen"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stdout(&first).contains("gen a") && stdout(&first).contains("gen b"));

    let second = mq_task(&dir, &["-f", "tasks.md", "gen"]);
    assert!(second.status.success());
    assert!(!stdout(&second).contains("gen a"), "{}", stdout(&second));
    assert!(stdout(&second).contains("up to date"));
}

#[test]
fn ignore_error_lets_the_run_continue_with_exit_zero() {
    let dir = workdir("ignore-error");
    write(
        &dir,
        "tasks.md",
        r#"
## flaky

```meta
ignore_error = true
```

```bash
echo flaky-ran
exit 5
```

## ci

```meta
depends = ["flaky"]
```

```bash
echo ci-ran
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "ci"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("ci-ran"));
    assert!(stderr(&out).contains("ignoring error"), "{}", stderr(&out));

    // Running it directly succeeds too.
    let out = mq_task(&dir, &["-f", "tasks.md", "flaky"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn ignore_error_applies_after_retries_and_is_not_cached() {
    let dir = workdir("ignore-error-retry");
    fs::write(dir.join("in.txt"), "x").unwrap();
    write(
        &dir,
        "tasks.md",
        r#"
## flaky

```meta
ignore_error = true
retries = 1
retry_delay = "10ms"
sources = ["in.txt"]
```

```bash
echo attempt
exit 1
```
"#,
    );
    let first = mq_task(&dir, &["-f", "tasks.md", "flaky"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert_eq!(stdout(&first).matches("attempt").count(), 2);

    // The ignored failure is not remembered as "up to date".
    let second = mq_task(&dir, &["-f", "tasks.md", "flaky"]);
    assert_eq!(stdout(&second).matches("attempt").count(), 2);
}

#[test]
fn ignore_error_does_not_hide_configuration_errors() {
    let dir = workdir("ignore-error-config");
    write(
        &dir,
        "tasks.md",
        r#"
## needs-param

```meta
ignore_error = true
params = ["required"]
```

```bash
true
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "needs-param"]);
    assert!(!out.status.success());
}

/// Run `mq-task` with `input` written to its stdin.
fn mq_task_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

const PROMPT_TASKS: &str = r#"
## deploy

```meta
prompt = "Deploy to production?"
```

```bash
echo deploying
```

## each

```meta
prompt = "Run for all?"
for = ["a", "b", "c"]
```

```bash
echo "item $MX_ITEM"
```
"#;

#[test]
fn prompt_runs_the_task_only_when_confirmed() {
    let dir = workdir("prompt");
    write(&dir, "tasks.md", PROMPT_TASKS);

    let yes = mq_task_stdin(&dir, &["-f", "tasks.md", "deploy"], "y\n");
    assert!(yes.status.success(), "{}", stderr(&yes));
    assert!(stdout(&yes).contains("deploying"));
    assert!(stderr(&yes).contains("Deploy to production?"));

    let no = mq_task_stdin(&dir, &["-f", "tasks.md", "deploy"], "n\n");
    assert!(!no.status.success());
    assert!(!stdout(&no).contains("deploying"));
    assert!(stderr(&no).contains("not confirmed"), "{}", stderr(&no));

    // EOF counts as "no".
    let eof = mq_task(&dir, &["-f", "tasks.md", "deploy"]);
    assert!(!eof.status.success());
    assert!(!stdout(&eof).contains("deploying"));
}

#[test]
fn yes_flag_skips_prompts() {
    let dir = workdir("prompt-yes");
    write(&dir, "tasks.md", PROMPT_TASKS);
    for flag in ["--yes", "-y"] {
        let out = mq_task(&dir, &["-f", "tasks.md", "deploy", flag]);
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(stdout(&out).contains("deploying"));
    }
}

#[test]
fn a_looped_task_is_confirmed_once() {
    let dir = workdir("prompt-loop");
    write(&dir, "tasks.md", PROMPT_TASKS);
    let out = mq_task_stdin(&dir, &["-f", "tasks.md", "each"], "y\n");
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for item in ["a", "b", "c"] {
        assert!(text.contains(&format!("item {}", item)), "{}", text);
    }
}

#[test]
fn prompts_cannot_be_asked_in_parallel_runs() {
    let dir = workdir("prompt-parallel");
    write(
        &dir,
        "tasks.md",
        r#"
## deploy

```meta
prompt = "Sure?"
```

```bash
echo deploying
```

## all

```meta
depends = ["deploy"]
parallel = true
```

```bash
echo done
```
"#,
    );
    let out = mq_task_stdin(&dir, &["-f", "tasks.md", "all"], "y\n");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--yes"), "{}", stderr(&out));
    assert!(!stdout(&out).contains("deploying"));

    let out = mq_task(&dir, &["-f", "tasks.md", "all", "--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("deploying"));
}

#[test]
fn dry_run_does_not_prompt() {
    let dir = workdir("prompt-dry");
    write(&dir, "tasks.md", PROMPT_TASKS);
    let out = mq_task(&dir, &["-f", "tasks.md", "deploy", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("[dry-run] prompt: Deploy to production?"));
}

#[test]
fn a_skipped_task_does_not_prompt() {
    let dir = workdir("prompt-skipped");
    write(
        &dir,
        "tasks.md",
        r#"
## deploy

```meta
prompt = "Sure?"
if = 'flag("NEVER_SET_FLAG")'
```

```bash
echo deploying
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "deploy"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stderr(&out).contains("Sure?"));
}

#[test]
fn dotenv_files_provide_task_environment() {
    let dir = workdir("dotenv");
    write(
        &dir,
        ".env",
        "FROM_ENV_FILE=file\nSHARED=from-dotenv\nQUOTED=\"a b\"\n# comment\n",
    );
    write(&dir, ".env.local", "FROM_ENV_FILE=local\n");
    write(
        &dir,
        "tasks.md",
        r#"
```meta
dotenv = [".env"]
```

## show

```meta
dotenv = [".env.local", ".env.missing"]
env = ["SHARED=from-meta"]
```

```bash
echo "file=$FROM_ENV_FILE shared=$SHARED quoted=$QUOTED"
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "show"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // Later files win, `meta` env beats dotenv, a missing file is ignored.
    assert!(
        stdout(&out).contains("file=local shared=from-meta quoted=a b"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn the_process_environment_and_cli_override_dotenv() {
    let dir = workdir("dotenv-precedence");
    write(&dir, ".env", "GREETING=from-dotenv\n");
    write(
        &dir,
        "tasks.md",
        r#"
## show

```meta
dotenv = [".env"]
```

```bash
echo "greeting=$GREETING"
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "show"]);
    assert!(stdout(&out).contains("greeting=from-dotenv"));

    let out = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(["-f", "tasks.md", "show"])
        .env("GREETING", "from-process")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(stdout(&out).contains("greeting=from-process"));

    let out = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(["-f", "tasks.md", "show", "--env", "GREETING=from-cli"])
        .env("GREETING", "from-process")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(stdout(&out).contains("greeting=from-cli"));
}

#[test]
fn dotenv_paths_are_relative_to_the_task_directory() {
    let dir = workdir("dotenv-dir");
    fs::create_dir_all(dir.join("svc")).unwrap();
    write(&dir.join("svc"), ".env", "WHERE=svc\n");
    write(
        &dir,
        "tasks.md",
        r#"
## show

```meta
dir = "svc"
dotenv = [".env"]
```

```bash
echo "where=$WHERE"
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "show"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("where=svc"));
}

#[test]
fn a_malformed_dotenv_file_is_an_error() {
    let dir = workdir("dotenv-bad");
    write(&dir, ".env", "GOOD=1\nthis is not valid\n");
    write(
        &dir,
        "tasks.md",
        "## show\n\n```meta\ndotenv = [\".env\"]\n```\n\n```bash\necho hi\n```\n",
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "show"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("line 2"), "{}", stderr(&out));
    assert!(!stdout(&out).contains("hi"));
}

#[test]
fn status_commands_decide_whether_a_task_is_up_to_date() {
    let dir = workdir("status");
    write(
        &dir,
        "tasks.md",
        r#"
## build

```meta
status = ["test -f out.txt", "grep -q done out.txt"]
```

```bash
echo building
echo done > out.txt
```
"#,
    );
    // No output yet: status fails, the task runs and creates the file.
    let first = mq_task(&dir, &["-f", "tasks.md", "build"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stdout(&first).contains("building"));

    // Every status command passes now: skipped.
    let second = mq_task(&dir, &["-f", "tasks.md", "build"]);
    assert!(second.status.success());
    assert!(!stdout(&second).contains("building"), "{}", stdout(&second));
    assert!(stdout(&second).contains("up to date"));

    // One failing command is enough to run the task again.
    fs::write(dir.join("out.txt"), "stale").unwrap();
    let third = mq_task(&dir, &["-f", "tasks.md", "build"]);
    assert!(stdout(&third).contains("building"));

    // --no-cache ignores status.
    let forced = mq_task(&dir, &["-f", "tasks.md", "build", "--no-cache"]);
    assert!(stdout(&forced).contains("building"));
}

#[test]
fn status_runs_in_the_task_directory_with_its_environment() {
    let dir = workdir("status-env");
    fs::create_dir_all(dir.join("svc")).unwrap();
    write(&dir.join("svc"), "marker", "");
    write(
        &dir,
        "tasks.md",
        r#"
## check

```meta
dir = "svc"
env = ["WANT=1"]
status = ['test -f marker && test "$WANT" = 1']
```

```bash
echo ran
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "check"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stdout(&out).contains("ran"), "{}", stdout(&out));
}

#[test]
fn a_once_task_runs_a_single_time_for_different_calls() {
    let dir = workdir("once");
    write(
        &dir,
        "tasks.md",
        r#"
## setup

```meta
once = true
params = ["env"]
```

```bash
echo "setup $MX_PARAM_ENV"
```

## ci

```meta
depends = ["setup a", "setup b"]
```

```bash
echo done
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "ci"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    // Only the first call ran: its output appears once and `b` never does.
    assert_eq!(text.matches("setup a\n").count(), 1, "{}", text);
    assert!(
        !text.contains("setup b") && !text.contains("setup[b]"),
        "{}",
        text
    );
}

#[test]
fn summary_describes_a_task_without_running_it() {
    let dir = workdir("summary");
    write(
        &dir,
        "tasks.md",
        r#"
## deploy

Deploy the app to an environment.

```meta
alias = ["d"]
depends = ["build"]
params = ["env=staging", "region"]
retries = 2
```

```bash
echo SHOULD-NOT-RUN
```

## build

```bash
true
```
"#,
    );
    for args in [
        vec!["-f", "tasks.md", "deploy", "--summary"],
        vec!["run", "-f", "tasks.md", "d", "--summary"],
    ] {
        let out = mq_task(&dir, &args);
        assert!(out.status.success(), "{}", stderr(&out));
        let text = stdout(&out);
        assert!(
            text.starts_with("deploy\n  Deploy the app to an environment.\n"),
            "{}",
            text
        );
        assert!(text.contains("aliases  d"), "{}", text);
        assert!(text.contains("depends  build"), "{}", text);
        assert!(text.contains("env=staging, region (required)"), "{}", text);
        assert!(text.contains("retries: 2"), "{}", text);
        assert!(!text.contains("SHOULD-NOT-RUN"));
    }

    let missing = mq_task(&dir, &["-f", "tasks.md", "nope", "--summary"]);
    assert!(!missing.status.success());
}

#[test]
fn list_json_prints_machine_readable_tasks() {
    let dir = workdir("list-json");
    write(
        &dir,
        "tasks.md",
        r#"
## build

Compile everything.

```meta
alias = ["b"]
params = ["profile=dev"]
```

```bash
true
```

## _hidden

```bash
true
```
"#,
    );
    let out = mq_task(&dir, &["list", "-f", "tasks.md", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let tasks = value.as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["name"], "build");
    assert_eq!(tasks[0]["description"], "Compile everything.");
    assert_eq!(tasks[0]["aliases"][0], "b");
    assert_eq!(tasks[0]["params"][0]["name"], "profile");

    let all = mq_task(&dir, &["list", "-f", "tasks.md", "--json", "--all"]);
    let value: serde_json::Value = serde_json::from_str(&stdout(&all)).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 2);
}

#[test]
fn requires_checks_cli_env_dotenv_and_process_environment() {
    let dir = workdir("requires");
    write(&dir, ".env", "FROM_DOTENV=1\n");
    write(
        &dir,
        "tasks.md",
        r#"
## deploy

```meta
requires = ["API_TOKEN", "FROM_DOTENV"]
dotenv = [".env"]
```

```bash
echo "deploying with $API_TOKEN"
```
"#,
    );
    let missing = mq_task(&dir, &["-f", "tasks.md", "deploy"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("API_TOKEN"),
        "{}",
        stderr(&missing)
    );
    assert!(!stderr(&missing).contains("FROM_DOTENV"));
    assert!(!stdout(&missing).contains("deploying"));

    let cli = mq_task(
        &dir,
        &["-f", "tasks.md", "deploy", "--env", "API_TOKEN=abc"],
    );
    assert!(cli.status.success(), "{}", stderr(&cli));
    assert!(stdout(&cli).contains("deploying with abc"));

    let process = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(["-f", "tasks.md", "deploy"])
        .env("API_TOKEN", "from-process")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(stdout(&process).contains("deploying with from-process"));
}

#[test]
fn an_invalid_watch_interval_is_rejected() {
    let dir = workdir("interval");
    write(&dir, "tasks.md", "## t\n\n```bash\ntrue\n```\n");
    let out = mq_task(
        &dir,
        &["-f", "tasks.md", "t", "--watch", "--interval", "soon"],
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("Invalid duration"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn watch_reruns_when_a_source_changes_using_the_interval() {
    use std::process::Stdio;
    let dir = workdir("watch-interval");
    fs::write(dir.join("in.txt"), "1").unwrap();
    write(
        &dir,
        "tasks.md",
        r#"
## gen

```meta
sources = ["in.txt"]
```

```bash
echo generating
```
"#,
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(["-f", "tasks.md", "gen", "--watch", "--interval", "50ms"])
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    fs::write(dir.join("in.txt"), "2").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    let runs = stdout(&out).matches("generating").count();
    assert_eq!(runs, 2, "{}", stdout(&out));
}

const OUTPUT_TASKS: &str = r#"
## a

```bash
echo a1
sleep 0.3
echo a2
```

## b

```bash
echo b1
sleep 0.1
echo b2
```

## all

```meta
depends = ["a", "b"]
parallel = true
```

```bash
echo done
```
"#;

#[test]
fn output_group_prints_each_task_as_one_block() {
    let dir = workdir("output-group");
    write(&dir, "tasks.md", OUTPUT_TASKS);
    let out = mq_task(&dir, &["-f", "tasks.md", "all", "--output", "group"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    // b finishes first, but each block is contiguous.
    let a1 = text.find("a1\na2\n").expect(&text);
    let b1 = text.find("b1\nb2\n").expect(&text);
    assert!(b1 < a1, "{}", text);
    assert!(text.contains("▶ a\n") && text.contains("▶ b\n"));
    assert!(text.contains("✔ a") && text.contains("✔ b"));
    assert!(text.find("done").unwrap() > a1);
}

#[test]
fn output_group_also_applies_to_sequential_runs() {
    let dir = workdir("output-group-seq");
    write(&dir, "tasks.md", OUTPUT_TASKS);
    let out = mq_task(
        &dir,
        &["-f", "tasks.md", "all", "--output", "group", "-j", "1"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("▶ a\na1\na2\n✔ a"), "{}", text);
    assert!(text.contains("▶ b\nb1\nb2\n✔ b"), "{}", text);
}

#[test]
fn output_prefixed_tags_lines_even_when_sequential() {
    let dir = workdir("output-prefixed");
    write(&dir, "tasks.md", OUTPUT_TASKS);
    let out = mq_task(&dir, &["-f", "tasks.md", "a", "--output", "prefixed"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("a │ a1"), "{}", stdout(&out));
}

#[test]
fn output_interleaved_never_prefixes() {
    let dir = workdir("output-interleaved");
    write(&dir, "tasks.md", OUTPUT_TASKS);
    let out = mq_task(&dir, &["-f", "tasks.md", "all", "--output", "interleaved"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains('│'), "{}", text);
    assert!(text.contains("a1") && text.contains("b2") && text.contains("done"));
}

#[test]
fn an_unknown_output_mode_is_rejected() {
    let dir = workdir("output-bad");
    write(&dir, "tasks.md", OUTPUT_TASKS);
    let out = mq_task(&dir, &["-f", "tasks.md", "a", "--output", "loud"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("unknown output mode"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn for_from_runs_once_per_line_of_a_dependencys_output() {
    let dir = workdir("for-from");
    write(
        &dir,
        "tasks.md",
        r#"
## list-files

```meta
capture = true
```

```bash
printf 'one file.txt\ntwo.txt\n'
```

## process

```meta
depends = ["list-files"]
for = { from = "list-files" }
as = "file"
```

```bash
echo "processing [$MX_FILE] #$MX_INDEX"
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "process"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let first = text.find("processing [one file.txt] #0").expect(&text);
    let second = text.find("processing [two.txt] #1").expect(&text);
    assert!(first < second);
    assert!(text.contains("↳ process[one file.txt]"), "{}", text);
}

#[test]
fn for_from_with_no_output_is_skipped_and_failures_stop_the_loop() {
    let dir = workdir("for-from-edge");
    write(
        &dir,
        "tasks.md",
        r#"
## nothing

```meta
capture = true
```

```bash
true
```

## empty

```meta
depends = ["nothing"]
for = { from = "nothing" }
```

```bash
echo should-not-run
```

## three

```meta
capture = true
```

```bash
printf 'a\nb\nc\n'
```

## stops

```meta
depends = ["three"]
for = { from = "three" }
```

```bash
echo "ran $MX_ITEM"
[ "$MX_ITEM" != b ] || exit 6
```
"#,
    );
    let empty = mq_task(&dir, &["-f", "tasks.md", "empty"]);
    assert!(empty.status.success(), "{}", stderr(&empty));
    assert!(!stdout(&empty).contains("should-not-run"));
    assert!(stdout(&empty).contains("no items"), "{}", stdout(&empty));

    let stops = mq_task(&dir, &["-f", "tasks.md", "stops"]);
    assert_eq!(stops.status.code(), Some(6));
    let text = stdout(&stops);
    assert!(text.contains("ran a") && text.contains("ran b") && !text.contains("ran c"));
}

#[test]
fn for_from_must_name_a_capturing_dependency() {
    let dir = workdir("for-from-invalid");
    write(
        &dir,
        "tasks.md",
        r#"
## list

```bash
echo x
```

## each

```meta
depends = ["list"]
for = { from = "list" }
```

```bash
true
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "each"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("capture = true"), "{}", stderr(&out));
}

#[test]
fn for_from_works_across_imports() {
    let dir = workdir("for-from-import");
    write(
        &dir,
        "lib.md",
        r#"
## names

```meta
capture = true
```

```bash
printf 'x\ny\n'
```

## each

```meta
depends = ["names"]
for = { from = "names" }
```

```bash
echo "imported $MX_ITEM"
```
"#,
    );
    write(
        &dir,
        "tasks.md",
        r#"
```meta
imports = ["lib.md"]
```

## go

```meta
depends = ["lib:each"]
```

```bash
echo finished
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "go"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("imported x") && text.contains("imported y"),
        "{}",
        text
    );
}

#[test]
fn defer_runs_after_every_loop_item() {
    let dir = workdir("defer-loop");
    write(
        &dir,
        "tasks.md",
        r#"
## cleanup

```bash
echo "cleanup after $MX_ITEM"
```

## each

```meta
for = ["a", "b"]
defer = ["cleanup"]
```

```bash
echo "run $MX_ITEM"
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "each"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let lines: Vec<String> = stdout(&out)
        .lines()
        .filter(|l| l.starts_with("run ") || l.starts_with("cleanup "))
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        vec!["run a", "cleanup after a", "run b", "cleanup after b"]
    );
}

#[test]
fn dependencies_can_be_called_by_alias_with_arguments() {
    let dir = workdir("alias-args");
    write(
        &dir,
        "tasks.md",
        r#"
## deploy

```meta
alias = ["d"]
params = ["env"]
```

```bash
echo "deploying $MX_PARAM_ENV"
```

## ci

```meta
depends = ["d staging", "deploy prod"]
```

```bash
echo done
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "ci"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("deploying staging") && text.contains("deploying prod"),
        "{}",
        text
    );
}

#[test]
fn output_group_reports_failures_with_their_output() {
    let dir = workdir("output-group-fail");
    write(
        &dir,
        "tasks.md",
        r#"
## broken

```bash
echo before
echo oops >&2
exit 3
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "broken", "--output", "group"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stdout(&out).contains("▶ broken\nbefore"),
        "{}",
        stdout(&out)
    );
    assert!(stderr(&out).contains("oops"), "{}", stderr(&out));
    assert!(stderr(&out).contains("✘ broken"), "{}", stderr(&out));
}

#[test]
fn output_group_hides_the_output_of_skipped_tasks() {
    let dir = workdir("output-group-skip");
    fs::write(dir.join("in.txt"), "x").unwrap();
    write(
        &dir,
        "tasks.md",
        r#"
## gen

```meta
sources = ["in.txt"]
```

```bash
echo generating
```
"#,
    );
    let args = ["-f", "tasks.md", "gen", "--output", "group"];
    assert!(stdout(&mq_task(&dir, &args)).contains("generating"));
    let second = mq_task(&dir, &args);
    assert!(second.status.success());
    assert!(!stdout(&second).contains("generating"));
    assert!(stdout(&second).contains("up to date"));
}

#[test]
fn an_imported_files_dotenv_applies_to_its_tasks() {
    let dir = workdir("import-dotenv");
    fs::create_dir_all(dir.join("lib")).unwrap();
    write(&dir.join("lib"), ".env", "FROM_LIB=yes\n");
    write(
        &dir.join("lib"),
        "tools.md",
        r#"
```meta
dotenv = [".env"]
dir = "lib"
```

## show

```bash
echo "lib=$FROM_LIB"
```
"#,
    );
    write(
        &dir,
        "tasks.md",
        r#"
```meta
imports = ["lib/tools.md"]
```

## go

```meta
depends = ["tools:show"]
```

```bash
echo done
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "go"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("lib=yes"), "{}", stdout(&out));
}

#[test]
fn requires_is_not_checked_in_a_dry_run() {
    let dir = workdir("requires-dry");
    write(
        &dir,
        "tasks.md",
        "## t\n\n```meta\nrequires = [\"MQ_TASK_SURELY_UNSET\"]\n```\n\n```bash\necho hi\n```\n",
    );
    let dry = mq_task(&dir, &["-f", "tasks.md", "t", "--dry-run"]);
    assert!(dry.status.success(), "{}", stderr(&dry));
    let real = mq_task(&dir, &["-f", "tasks.md", "t"]);
    assert!(!real.status.success());
}

#[test]
fn a_timeout_can_be_ignored_too() {
    let dir = workdir("ignore-timeout");
    write(
        &dir,
        "tasks.md",
        r#"
## slow

```meta
ignore_error = true
timeout = "200ms"
```

```bash
sleep 5
```

## after

```meta
depends = ["slow"]
```

```bash
echo after-ran
```
"#,
    );
    let out = mq_task(&dir, &["-f", "tasks.md", "after"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("after-ran"));
    assert!(stderr(&out).contains("ignoring error"), "{}", stderr(&out));
}

#[test]
fn requires_accepts_variables_generated_for_the_task_process() {
    let dir = workdir("requires-generated");
    write(
        &dir,
        "tasks.md",
        r#"
## deploy

```meta
requires = ["MX_ARG_0"]
```

```bash
echo "deploying $MX_ARG_0"
```

## missing

```meta
requires = ["MX_ARG_1"]
```

```bash
echo should-not-run
```

## list-files

```meta
capture = true
```

```bash
printf 'a\nb\n'
```

## each

```meta
depends = ["list-files"]
for = { from = "list-files" }
as = "file"
requires = ["MX_ITEM", "MX_FILE"]
```

```bash
echo "item $MX_FILE"
```
"#,
    );
    let ok = mq_task(&dir, &["-f", "tasks.md", "deploy", "--", "prod"]);
    assert!(ok.status.success(), "{}", stderr(&ok));
    assert!(stdout(&ok).contains("deploying prod"));

    let missing = mq_task(&dir, &["-f", "tasks.md", "missing", "--", "prod"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("MX_ARG_1"),
        "{}",
        stderr(&missing)
    );
    assert!(!stdout(&missing).contains("should-not-run"));

    let each = mq_task(&dir, &["-f", "tasks.md", "each"]);
    assert!(each.status.success(), "{}", stderr(&each));
    let text = stdout(&each);
    assert!(
        text.contains("item a") && text.contains("item b"),
        "{}",
        text
    );
}

fn git_root(dir: &Path) {
    fs::create_dir_all(dir.join(".git")).unwrap();
}

#[test]
fn tasks_md_is_preferred_over_readme() {
    let dir = workdir("discover-prefer");
    write(
        &dir,
        "README.md",
        "## from-readme\n\n```bash\necho readme\n```\n",
    );
    write(
        &dir,
        "tasks.md",
        "## from-tasks\n\n```bash\necho tasks\n```\n",
    );

    let list = mq_task(&dir, &[]);
    assert!(list.status.success(), "{}", stderr(&list));
    assert!(stdout(&list).contains("from-tasks"), "{}", stdout(&list));
    assert!(!stdout(&list).contains("from-readme"));

    let run = mq_task(&dir, &["from-tasks"]);
    assert!(stdout(&run).contains("tasks"), "{}", stdout(&run));
}

#[test]
fn the_readme_is_the_fallback_task_file() {
    let dir = workdir("discover-readme");
    write(
        &dir,
        "README.md",
        "## hello\n\n```bash\necho from-readme\n```\n",
    );
    let out = mq_task(&dir, &["hello"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("from-readme"));
}

#[test]
fn tasks_are_found_from_a_subdirectory_and_run_in_the_project_root() {
    let dir = workdir("discover-up");
    git_root(&dir);
    write(&dir, "tasks.md", "## where\n\n```bash\npwd\n```\n");
    let sub = dir.join("a/b");
    fs::create_dir_all(&sub).unwrap();

    let out = mq_task(&sub, &["where"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let root = dir.canonicalize().unwrap();
    assert!(
        stdout(&out).lines().any(|l| Path::new(l.trim()) == root),
        "{}",
        stdout(&out)
    );
    assert!(stderr(&out).contains("using"), "{}", stderr(&out));
}

#[test]
fn relative_options_keep_pointing_at_the_invocation_directory() {
    let dir = workdir("discover-relative");
    git_root(&dir);
    write(&dir, "tasks.md", "## where\n\n```bash\npwd\n```\n");
    let sub = dir.join("sub");
    fs::create_dir_all(sub.join("work")).unwrap();

    let out = mq_task(&sub, &["where", "--dir", "work"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let expected = sub.join("work").canonicalize().unwrap();
    assert!(
        stdout(&out)
            .lines()
            .any(|l| Path::new(l.trim()) == expected),
        "{}",
        stdout(&out)
    );
}

#[test]
fn an_explicit_file_is_not_searched_for_and_keeps_the_directory() {
    let dir = workdir("discover-explicit");
    git_root(&dir);
    write(&dir, "tasks.md", "## from-tasks\n\n```bash\necho t\n```\n");
    write(&dir, "other.md", "## from-other\n\n```bash\npwd\n```\n");
    let sub = dir.join("sub");
    fs::create_dir_all(&sub).unwrap();
    write(&sub, "mine.md", "## mine\n\n```bash\npwd\n```\n");

    let out = mq_task(&sub, &["-f", "mine.md", "mine"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let expected = sub.canonicalize().unwrap();
    assert!(
        stdout(&out)
            .lines()
            .any(|l| Path::new(l.trim()) == expected)
    );
    assert!(!stderr(&out).contains("using"));
}

#[test]
fn mq_task_file_selects_the_task_file() {
    let dir = workdir("discover-env");
    write(&dir, "tasks.md", "## from-tasks\n\n```bash\necho t\n```\n");
    write(&dir, "ci.md", "## from-ci\n\n```bash\necho ci\n```\n");
    let out = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .arg("from-ci")
        .env("MQ_TASK_FILE", "ci.md")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("ci"));
}

#[test]
fn a_missing_task_file_names_what_was_searched() {
    let dir = workdir("discover-none");
    let out = mq_task(&dir, &["anything"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("No task file found") && err.contains("tasks.md"),
        "{}",
        err
    );
    assert!(err.contains("README.md") && err.contains("-f"), "{}", err);
}

#[test]
fn mq_task_toml_is_loaded_automatically() {
    let dir = workdir("config-auto");
    write(
        &dir,
        "tasks.md",
        "## build\n\n```bash\necho building\n```\n",
    );
    write(&dir, "mq-task.toml", "default_task = \"build\"\n");
    let out = mq_task(&dir, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("building"), "{}", stdout(&out));
}

#[test]
fn the_config_beside_the_task_file_is_used_for_an_explicit_file() {
    let dir = workdir("config-beside");
    fs::create_dir_all(dir.join("docs")).unwrap();
    write(
        &dir.join("docs"),
        "dev.md",
        "## build\n\n```bash\necho building\n```\n",
    );
    write(
        &dir.join("docs"),
        "mq-task.toml",
        "default_task = \"build\"\n",
    );
    let out = mq_task(&dir, &["-f", "docs/dev.md"]);
    assert!(stdout(&out).contains("building"), "{}", stdout(&out));
}

#[test]
fn init_writes_the_file_that_is_loaded_automatically() {
    let dir = workdir("config-init");
    write(&dir, "tasks.md", "## hi\n\n```bash\necho hi\n```\n");
    let init = mq_task(&dir, &["init"]);
    assert!(init.status.success(), "{}", stderr(&init));
    assert!(dir.join("mq-task.toml").is_file());
    assert!(!dir.join("mq_task.toml").exists());
    let run = mq_task(&dir, &["hi"]);
    assert!(run.status.success(), "{}", stderr(&run));
}

#[test]
fn a_config_with_unknown_keys_is_rejected() {
    let dir = workdir("config-typo");
    write(&dir, "tasks.md", "## hi\n\n```bash\necho hi\n```\n");
    write(&dir, "mq-task.toml", "defualt_task = \"hi\"\n");
    let out = mq_task(&dir, &["hi"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("defualt_task"), "{}", stderr(&out));
}

#[test]
fn a_typo_in_meta_is_an_error_with_a_suggestion() {
    let dir = workdir("meta-typo");
    write(
        &dir,
        "tasks.md",
        "## flaky\n\n```meta\nretires = 3\n```\n\n```bash\necho ran\n```\n",
    );
    let out = mq_task(&dir, &["flaky"]);
    assert!(!out.status.success());
    assert!(!stdout(&out).contains("ran"), "the task must not run");
    let err = stderr(&out);
    assert!(err.contains("unknown key `retires`"), "{}", err);
    assert!(err.contains("did you mean `retries`?"), "{}", err);
}

#[test]
fn an_unknown_task_suggests_a_similar_one() {
    let dir = workdir("task-hint");
    write(
        &dir,
        "tasks.md",
        r#"
## deploy

```meta
alias = ["ship"]
```

```bash
echo deploying
```

## ci

```meta
depends = ["deploi staging"]
```

```bash
echo ci
```
"#,
    );
    let out = mq_task(&dir, &["deploi"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("did you mean `deploy`?"),
        "{}",
        stderr(&out)
    );

    // Aliases are candidates too.
    let out = mq_task(&dir, &["shi"]);
    assert!(
        stderr(&out).contains("did you mean `ship`?"),
        "{}",
        stderr(&out)
    );

    // A mistyped dependency in `depends` is reported with the same hint.
    let out = mq_task(&dir, &["ci"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("did you mean `deploy`?"),
        "{}",
        stderr(&out)
    );

    // Nothing close: no hint.
    let out = mq_task(&dir, &["zzzzzz"]);
    assert!(stderr(&out).contains("Section not found: zzzzzz"));
    assert!(!stderr(&out).contains("did you mean"));

    let out = mq_task(&dir, &["deploi", "--summary"]);
    assert!(
        stderr(&out).contains("did you mean `deploy`?"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_failure_names_the_task_that_failed_and_keeps_its_exit_code() {
    let dir = workdir("fail-context");
    write(
        &dir,
        "tasks.md",
        r#"
## build

```bash
exit 4
```

## ci

```meta
depends = ["build"]
```

```bash
echo ci
```

## each

```meta
for = ["a", "b"]
```

```bash
[ "$MX_ITEM" != b ] || exit 5
```
"#,
    );
    let out = mq_task(&dir, &["ci"]);
    assert_eq!(out.status.code(), Some(4));
    assert!(
        stderr(&out).contains("status code 4 (in task 'build')"),
        "{}",
        stderr(&out)
    );

    let out = mq_task(&dir, &["each"]);
    assert_eq!(out.status.code(), Some(5));
    assert!(
        stderr(&out).contains("(in task 'each[b]')"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_missing_runtime_names_the_task_and_a_timeout_is_not_doubled() {
    let dir = workdir("fail-context-kinds");
    write(
        &dir,
        "tasks.md",
        r#"
## cobol-task

```cobol
DISPLAY 'hi'.
```

## slow

```meta
timeout = "100ms"
```

```bash
sleep 5
```
"#,
    );
    let out = mq_task(&dir, &["cobol-task"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("Runtime not found for language: cobol (in task 'cobol-task')"),
        "{}",
        stderr(&out)
    );

    let out = mq_task(&dir, &["slow"]);
    assert_eq!(out.status.code(), Some(124));
    assert!(
        stderr(&out).contains("Task 'slow' timed out"),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("(in task"), "{}", stderr(&out));
}
