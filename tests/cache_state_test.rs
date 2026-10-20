//! `cache::record` / `cache::is_fresh`.
//!
//! The cache lives in `./.mq-task/cache.json`, relative to the process's
//! current directory, so everything runs inside one test that owns the CWD.

use std::fs;
use std::path::Path;

use mq_task::cache::{is_fresh, record};
use mq_task::section::Section;

fn section(title: &str, outputs: &[&str]) -> Section {
    Section {
        title: title.into(),
        outputs: outputs.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn cache_round_trip() {
    let work = tempfile::tempdir().unwrap();
    std::env::set_current_dir(work.path()).unwrap();
    let dir = Some(work.path());
    let plain = section("build", &[]);

    // Nothing recorded yet.
    assert!(!is_fresh("scope", &plain, "fp1", dir));

    // A recorded fingerprint is fresh; any other is not.
    record("scope", "build", "fp1");
    assert!(Path::new(".mq-task/cache.json").is_file());
    assert!(is_fresh("scope", &plain, "fp1", dir));
    assert!(!is_fresh("scope", &plain, "fp2", dir));

    // Entries are keyed by scope and title.
    assert!(!is_fresh("other-scope", &plain, "fp1", dir));
    assert!(!is_fresh("scope", &section("test", &[]), "fp1", dir));

    // Recording another task keeps earlier entries.
    record("scope", "test", "fpT");
    assert!(is_fresh("scope", &plain, "fp1", dir));
    assert!(is_fresh("scope", &section("test", &[]), "fpT", dir));

    // Re-recording replaces the fingerprint.
    record("scope", "build", "fp2");
    assert!(!is_fresh("scope", &plain, "fp1", dir));
    assert!(is_fresh("scope", &plain, "fp2", dir));

    // Declared outputs must exist for the task to count as fresh.
    let with_outputs = section("build", &["dist/*.bin"]);
    assert!(!is_fresh("scope", &with_outputs, "fp2", dir));
    fs::create_dir(work.path().join("dist")).unwrap();
    fs::write(work.path().join("dist/app.bin"), "x").unwrap();
    assert!(is_fresh("scope", &with_outputs, "fp2", dir));
    // ...and a wrong fingerprint is stale no matter what exists.
    assert!(!is_fresh("scope", &with_outputs, "nope", dir));

    // Every output pattern has to match something.
    let two_outputs = section("build", &["dist/*.bin", "dist/*.map"]);
    assert!(!is_fresh("scope", &two_outputs, "fp2", dir));

    // A corrupted cache file is treated as empty and repaired by `record`.
    fs::write(".mq-task/cache.json", "{ not json").unwrap();
    assert!(!is_fresh("scope", &plain, "fp2", dir));
    record("scope", "build", "fp3");
    assert!(is_fresh("scope", &plain, "fp3", dir));

    // No temp files are left behind.
    let leftovers: Vec<_> = fs::read_dir(".mq-task")
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
