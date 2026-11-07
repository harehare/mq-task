//! SIGTERM stops the running task, still runs its `defer` tasks, and makes
//! mq-task exit like a shell would (128 + signal).
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TASKS: &str = r#"
## main

```meta
defer = ["cleanup"]
```

```bash
echo started > started.txt
sleep 30
echo finished > finished.txt
```

## cleanup

```bash
echo cleaned > cleaned.txt
```
"#;

fn wait_for_file(path: &Path) {
    let end = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < end, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn terminate_during_main(signal: libc::c_int) -> (std::process::ExitStatus, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("tasks.md"), TASKS).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(["-f", "tasks.md", "main"])
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_file(&dir.path().join("started.txt"));
    // SAFETY: signalling the child we just started.
    unsafe { libc::kill(child.id() as libc::pid_t, signal) };

    let end = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            panic!("mq-task did not stop after the signal");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (status, dir)
}

#[test]
fn sigterm_stops_the_task_and_runs_defer() {
    let (status, dir) = terminate_during_main(libc::SIGTERM);
    assert_eq!(status.code(), Some(143));
    assert!(dir.path().join("cleaned.txt").exists(), "defer did not run");
    assert!(!dir.path().join("finished.txt").exists());
}

#[test]
fn sighup_is_handled_the_same_way() {
    let (status, dir) = terminate_during_main(libc::SIGHUP);
    assert_eq!(status.code(), Some(129));
    assert!(dir.path().join("cleaned.txt").exists(), "defer did not run");
}
