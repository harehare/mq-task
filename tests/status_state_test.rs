//! `Runner::task_states` on a task with `sources` and `outputs`. The cache
//! lives in the current directory, so this changes it and gets a test binary
//! of its own (see `cache_state_test.rs`).

use std::fs;

use mq_task::{Config, RunOptions, Runner};

#[test]
fn states_say_whether_a_task_would_run() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tasks.md");
    fs::write(
        &file,
        "## built\n\n```meta\nsources = [\"in.txt\"]\noutputs = [\"out.txt\"]\n```\n\n```bash\ncp in.txt out.txt\n```\n",
    )
    .unwrap();
    fs::write(dir.path().join("in.txt"), "in").unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    let tasks = vec!["built".to_string()];
    let mut r = Runner::new(Config::default());

    let before = r
        .task_states(&file, &tasks, &RunOptions::default())
        .unwrap();
    assert_eq!(before.len(), 1);
    assert!(!before[0].up_to_date, "{before:?}");
    // Checking does not run the task.
    assert!(!dir.path().join("out.txt").exists());

    r.run_task_with_options(&file, "built", RunOptions::default())
        .unwrap();
    let after = r
        .task_states(&file, &tasks, &RunOptions::default())
        .unwrap();
    assert!(after[0].up_to_date, "{after:?}");
    assert_eq!(after[0].detail, "up to date");
}
