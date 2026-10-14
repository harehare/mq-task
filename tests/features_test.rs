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
if = 'env.GO == "yes"'
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

