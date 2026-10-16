//! Up-to-date checks for tasks that declare `sources` (and optionally
//! `outputs`), plus change snapshots used by `--watch`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::runner::Section;
use crate::util::Fnv;

const CACHE_DIR: &str = ".mq-task";
const CACHE_FILE: &str = "cache.json";

/// Serializes read-modify-write cycles when tasks finish concurrently.
static CACHE_LOCK: Mutex<()> = Mutex::new(());

fn resolve(base: Option<&Path>, pattern: &str) -> String {
    match base {
        Some(dir) if !Path::new(pattern).is_absolute() => {
            dir.join(pattern).to_string_lossy().into_owned()
        }
        _ => pattern.to_string(),
    }
}

/// Expand glob patterns (relative to `dir`) into a sorted list of files.
pub fn expand(patterns: &[String], dir: Option<&Path>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for pattern in patterns {
        if let Ok(paths) = glob::glob(&resolve(dir, pattern)) {
            files.extend(paths.flatten().filter(|p| p.is_file()));
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Fingerprint of everything that should invalidate a task's cached result:
/// source contents, the code itself, arguments and environment.
pub fn fingerprint(
    section: &Section,
    env: &[(String, String)],
    args: &[String],
    dir: Option<&Path>,
) -> String {
    let mut h = Fnv::default();
    for block in &section.codes {
        h.field(block.lang.as_bytes());
        h.field(block.code.as_bytes());
    }
    for arg in args {
        h.field(arg.as_bytes());
    }
    let mut env: Vec<_> = env.iter().collect();
    env.sort();
    for (k, v) in env {
        h.field(k.as_bytes());
        h.field(v.as_bytes());
    }
    for path in expand(&section.sources, dir) {
        h.field(path.to_string_lossy().as_bytes());
        match fs::read(&path) {
            Ok(bytes) => h.field(&bytes),
            Err(_) => h.field(b"<unreadable>"),
        }
    }
    h.finish()
}

fn cache_path() -> PathBuf {
    Path::new(CACHE_DIR).join(CACHE_FILE)
}

fn load() -> BTreeMap<String, String> {
    fs::read_to_string(cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn key(scope: &str, title: &str) -> String {
    format!("{}::{}", scope, title)
}

/// True when the stored fingerprint matches and all declared `outputs` exist.
pub fn is_fresh(scope: &str, section: &Section, fingerprint: &str, dir: Option<&Path>) -> bool {
    is_fresh_named(scope, &section.title, section, fingerprint, dir)
}

/// [`is_fresh`] for a run recorded under `name` rather than the task title
/// (loop iterations and calls with arguments each have their own entry).
pub fn is_fresh_named(
    scope: &str,
    name: &str,
    section: &Section,
    fingerprint: &str,
    dir: Option<&Path>,
) -> bool {
    let _guard = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if load().get(&key(scope, name)).map(String::as_str) != Some(fingerprint) {
        return false;
    }
    section
        .outputs
        .iter()
        .all(|pattern| !expand(std::slice::from_ref(pattern), dir).is_empty())
}

/// Record a successful run. Failures to write the cache are ignored: caching
/// is an optimization and must never fail a task.
pub fn record(scope: &str, title: &str, fingerprint: &str) {
    let _guard = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = load();
    map.insert(key(scope, title), fingerprint.to_string());
    if fs::create_dir_all(CACHE_DIR).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_string_pretty(&map) {
        let tmp = Path::new(CACHE_DIR).join(format!("{}.{}.tmp", CACHE_FILE, std::process::id()));
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, cache_path());
        }
    }
}

/// Cheap change detector for `--watch`: path + mtime + size of every match.
pub fn snapshot(
    patterns: &[String],
    dir: Option<&Path>,
) -> HashMap<PathBuf, (std::time::SystemTime, u64)> {
    expand(patterns, dir)
        .into_iter()
        .filter_map(|p| {
            let md = fs::metadata(&p).ok()?;
            Some((p, (md.modified().ok()?, md.len())))
        })
        .collect()
}
