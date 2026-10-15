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
    let out = mq_task(&dir, &["completions", "zsh"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("mq-task"));
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
