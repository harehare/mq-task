//! `cache`: glob expansion, fingerprints and watch snapshots.
//!
//! Nothing here touches the process-wide current directory; the stateful
//! `record`/`is_fresh` pair is covered in `cache_state_test.rs`.

#[path = "common/helpers.rs"]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use mq_task::cache::{expand, fingerprint, snapshot, snapshot_files};
use mq_task::section::{CodeBlock, Section};
use proptest::prelude::*;
use rstest::{fixture, rstest};
use tempfile::TempDir;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    path
}

fn names(paths: &[PathBuf], base: &Path) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.strip_prefix(base).unwrap().to_string_lossy().into_owned())
        .collect()
}

#[fixture]
fn tree() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for f in [
        "a.rs",
        "b.rs",
        "c.txt",
        "src/lib.rs",
        "src/deep/mod.rs",
        "src/notes.md",
    ] {
        write(dir.path(), f, f);
    }
    fs::create_dir(dir.path().join("emptydir.rs")).unwrap();
    dir
}

#[rstest]
#[case::star(&["*.rs"], &["a.rs", "b.rs"])]
#[case::exact(&["c.txt"], &["c.txt"])]
#[case::nested_star(&["src/*.rs"], &["src/lib.rs"])]
#[case::recursive(&["src/**/*.rs"], &["src/deep/mod.rs", "src/lib.rs"])]
#[case::several_patterns(&["c.txt", "*.rs"], &["a.rs", "b.rs", "c.txt"])]
#[case::overlapping_patterns_dedup(&["*.rs", "a.*"], &["a.rs", "b.rs"])]
#[case::no_match(&["*.zig"], &[])]
#[case::no_patterns(&[], &[])]
#[case::invalid_glob_is_ignored(&["[", "c.txt"], &["c.txt"])]
#[case::negation_removes_matches(&["**/*.rs", "!src/**"], &["a.rs", "b.rs"])]
#[case::negation_may_come_first(&["!src/deep/**", "src/**/*.rs"], &["src/lib.rs"])]
#[case::negation_single_star_stays_in_the_directory(&["**/*.rs", "!*.rs"], &["src/deep/mod.rs", "src/lib.rs"])]
#[case::negation_alone_matches_nothing(&["!*.rs"], &[])]
fn expands_patterns_relative_to_dir(
    tree: TempDir,
    #[case] patterns: &[&str],
    #[case] expected: &[&str],
) {
    let got = expand(&strings(patterns), Some(tree.path()));
    assert_eq!(names(&got, tree.path()), expected);
}

#[rstest]
fn expand_skips_directories(tree: TempDir) {
    // `emptydir.rs` matches `*.rs` but is not a file.
    let got = expand(&strings(&["*.rs"]), Some(tree.path()));
    assert!(!names(&got, tree.path()).contains(&"emptydir.rs".to_string()));
}

#[rstest]
fn expand_returns_sorted_paths(tree: TempDir) {
    let got = expand(&strings(&["src/**/*", "*"]), Some(tree.path()));
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(got, sorted);
}

#[rstest]
fn absolute_patterns_ignore_dir(tree: TempDir) {
    let other = tempfile::tempdir().unwrap();
    let pattern = format!("{}/*.rs", tree.path().display());
    let got = expand(&[pattern], Some(other.path()));
    assert_eq!(names(&got, tree.path()), ["a.rs", "b.rs"]);
}

fn section(code: &str, sources: &[&str]) -> Section {
    Section {
        title: "t".into(),
        codes: vec![CodeBlock {
            lang: "bash".into(),
            code: code.into(),
        }],
        sources: strings(sources),
        ..Default::default()
    }
}

fn fp(s: &Section, env: &[(&str, &str)], args: &[&str], dir: Option<&Path>) -> String {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    fingerprint(s, &env, &strings(args), dir)
}

#[test]
fn fingerprint_is_stable() {
    let s = section("echo", &[]);
    assert_eq!(
        fp(&s, &[("A", "1")], &["x"], None),
        fp(&s, &[("A", "1")], &["x"], None)
    );
    assert_eq!(fp(&s, &[], &[], None).len(), 16);
}

#[test]
fn fingerprint_reacts_to_code_language_args_and_env() {
    let base = section("echo", &[]);
    let reference = fp(&base, &[("A", "1")], &["x"], None);

    let mut other_lang = base.clone();
    other_lang.codes[0].lang = "sh".into();

    let variants = [
        fp(&section("echo hi", &[]), &[("A", "1")], &["x"], None),
        fp(&other_lang, &[("A", "1")], &["x"], None),
        fp(&base, &[("A", "2")], &["x"], None),
        fp(&base, &[("B", "1")], &["x"], None),
        fp(&base, &[("A", "1"), ("B", "2")], &["x"], None),
        fp(&base, &[("A", "1")], &["y"], None),
        fp(&base, &[("A", "1")], &["x", "y"], None),
        fp(&base, &[("A", "1")], &[], None),
    ];
    for (i, v) in variants.iter().enumerate() {
        assert_ne!(v, &reference, "variant {i} did not change the fingerprint");
    }
}

#[test]
fn fingerprint_ignores_the_title() {
    let mut a = section("echo", &[]);
    let b = a.clone();
    a.title = "renamed".into();
    assert_eq!(fp(&a, &[], &[], None), fp(&b, &[], &[], None));
}

#[test]
fn fingerprint_is_insensitive_to_env_order() {
    let s = section("echo", &[]);
    assert_eq!(
        fp(&s, &[("A", "1"), ("B", "2")], &[], None),
        fp(&s, &[("B", "2"), ("A", "1")], &[], None)
    );
}

#[test]
fn fingerprint_is_sensitive_to_arg_order() {
    let s = section("echo", &[]);
    assert_ne!(
        fp(&s, &[], &["a", "b"], None),
        fp(&s, &[], &["b", "a"], None)
    );
}

#[test]
fn fingerprint_tracks_source_contents() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "in.txt", "one");
    let s = section("echo", &["*.txt"]);

    let before = fp(&s, &[], &[], Some(dir.path()));
    assert_eq!(before, fp(&s, &[], &[], Some(dir.path())));

    fs::write(&file, "two").unwrap();
    let edited = fp(&s, &[], &[], Some(dir.path()));
    assert_ne!(before, edited);

    write(dir.path(), "extra.txt", "new");
    let added = fp(&s, &[], &[], Some(dir.path()));
    assert_ne!(edited, added);

    fs::remove_file(dir.path().join("extra.txt")).unwrap();
    assert_eq!(fp(&s, &[], &[], Some(dir.path())), edited);
}

#[test]
fn fingerprint_ignores_files_outside_sources() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "in.txt", "one");
    let s = section("echo", &["in.txt"]);
    let before = fp(&s, &[], &[], Some(dir.path()));
    write(dir.path(), "unrelated.txt", "anything");
    assert_eq!(fp(&s, &[], &[], Some(dir.path())), before);
}

#[test]
fn fingerprint_distinguishes_adjacent_fields() {
    // ("ab", "c") vs ("a", "bc") must not collide.
    let s = section("echo", &[]);
    assert_ne!(
        fp(&s, &[], &["ab", "c"], None),
        fp(&s, &[], &["a", "bc"], None)
    );
    assert_ne!(
        fp(&s, &[("k", "vw")], &[], None),
        fp(&s, &[("kv", "w")], &[], None)
    );
}

proptest! {
    #[test]
    fn fingerprint_is_deterministic_and_env_order_independent(
        code in ".{0,20}",
        args in prop::collection::vec(".{0,6}", 0..4),
        env in prop::collection::btree_map("[A-Z]{1,4}", ".{0,6}", 0..5),
        seed in any::<u64>(),
    ) {
        let s = section(&code, &[]);
        let mut pairs: Vec<(String, String)> = env.into_iter().collect();
        let args: Vec<String> = args;
        let a = fingerprint(&s, &pairs, &args, None);
        // Deterministic shuffle.
        if !pairs.is_empty() {
            let n = pairs.len();
            pairs.rotate_left((seed as usize) % n);
            pairs.reverse();
        }
        let b = fingerprint(&s, &pairs, &args, None);
        prop_assert_eq!(a, b);
    }

    #[test]
    fn different_args_give_different_fingerprints(
        a in prop::collection::vec("[a-z]{0,4}", 0..4),
        b in prop::collection::vec("[a-z]{0,4}", 0..4),
    ) {
        prop_assume!(a != b);
        let s = section("echo", &[]);
        prop_assert_ne!(fingerprint(&s, &[], &a, None), fingerprint(&s, &[], &b, None));
    }

    #[test]
    fn different_code_gives_different_fingerprints(a in ".{0,12}", b in ".{0,12}") {
        prop_assume!(a != b);
        prop_assert_ne!(
            fingerprint(&section(&a, &[]), &[], &[], None),
            fingerprint(&section(&b, &[]), &[], &[], None)
        );
    }
}

#[test]
fn snapshot_lists_matches_with_size() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.txt", "12345");
    write(dir.path(), "skip.md", "x");
    let snap = snapshot(&strings(&["*.txt"]), Some(dir.path()));
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[&a].1, 5);
}

#[test]
fn snapshot_detects_changes_additions_and_removals() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.txt", "one");
    let patterns = strings(&["*.txt"]);

    let first = snapshot(&patterns, Some(dir.path()));
    assert_eq!(first, snapshot(&patterns, Some(dir.path())));

    // Size change is always visible, even within one mtime tick.
    fs::write(&a, "longer").unwrap();
    let edited = snapshot(&patterns, Some(dir.path()));
    assert_ne!(first, edited);

    let b = write(dir.path(), "b.txt", "x");
    let added = snapshot(&patterns, Some(dir.path()));
    assert_eq!(added.len(), 2);
    assert_ne!(edited, added);

    fs::remove_file(&b).unwrap();
    assert_eq!(snapshot(&patterns, Some(dir.path())).len(), 1);
}

#[test]
fn snapshot_of_nothing_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(snapshot(&strings(&["*.nope"]), Some(dir.path())).is_empty());
    assert!(snapshot(&[], Some(dir.path())).is_empty());
}

#[test]
fn snapshot_files_treats_names_literally_and_sees_new_files() {
    let dir = tempfile::tempdir().unwrap();
    let names = strings(&[".env[prod]"]);
    let before = snapshot_files(&names, Some(dir.path()));
    assert!(before.is_empty());

    let file = write(dir.path(), ".env[prod]", "A=1");
    let created = snapshot_files(&names, Some(dir.path()));
    assert_eq!(created.len(), 1);
    assert_ne!(before, created);

    // As a glob the bracket is a character class and misses the file.
    assert!(snapshot(&names, Some(dir.path())).is_empty());

    fs::write(&file, "A=22").unwrap();
    assert_ne!(created, snapshot_files(&names, Some(dir.path())));
}
