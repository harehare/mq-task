//! Finding the task file when `-f` is not given.
//!
//! A dedicated file (`tasks.md`) wins over `README.md`, and the search walks
//! up to the repository root so tasks run from any subdirectory.

use std::path::{Path, PathBuf};

/// Names tried first, in every directory, before falling back to `README.md`.
pub const DEDICATED: [&str; 3] = ["tasks.md", "Tasks.md", "TASKS.md"];
/// Used when no dedicated file exists.
pub const FALLBACK: &str = "README.md";

/// Directories to search: `start` and its parents up to the repository
/// root (a directory holding `.git`), or just `start` outside a repository.
fn search_dirs(start: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in start.ancestors() {
        dirs.push(dir.to_path_buf());
        if dir.join(".git").exists() {
            return dirs;
        }
    }
    vec![start.to_path_buf()]
}

/// The nearest `tasks.md`, else the nearest `README.md`.
pub fn find_task_file(start: &Path) -> Option<PathBuf> {
    let dirs = search_dirs(start);
    let dedicated = DEDICATED.iter().copied();
    let fallback = std::iter::once(FALLBACK);
    for names in [dedicated.collect::<Vec<_>>(), fallback.collect::<Vec<_>>()] {
        for dir in &dirs {
            for name in &names {
                let path = dir.join(name);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    #[test]
    fn a_dedicated_file_beats_the_readme_in_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("README.md"));
        touch(&dir.path().join("tasks.md"));
        assert_eq!(
            find_task_file(dir.path()),
            Some(dir.path().join("tasks.md"))
        );
    }

    #[test]
    fn the_readme_is_used_when_there_is_no_dedicated_file() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("README.md"));
        assert_eq!(
            find_task_file(dir.path()),
            Some(dir.path().join("README.md"))
        );
        assert_eq!(find_task_file(&dir.path().join("empty")), None);
    }

    #[test]
    fn the_search_walks_up_to_the_repository_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".git")).unwrap();
        touch(&root.path().join("tasks.md"));
        let deep = root.path().join("a/b");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_task_file(&deep), Some(root.path().join("tasks.md")));
    }

    #[test]
    fn a_dedicated_file_above_beats_a_nearer_readme() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".git")).unwrap();
        touch(&root.path().join("tasks.md"));
        touch(&root.path().join("docs/README.md"));
        assert_eq!(
            find_task_file(&root.path().join("docs")),
            Some(root.path().join("tasks.md"))
        );
    }

    #[test]
    fn the_nearest_readme_wins_without_a_dedicated_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".git")).unwrap();
        touch(&root.path().join("README.md"));
        touch(&root.path().join("docs/README.md"));
        assert_eq!(
            find_task_file(&root.path().join("docs")),
            Some(root.path().join("docs/README.md"))
        );
    }

    #[test]
    fn the_search_stops_at_the_repository_root() {
        let outer = tempfile::tempdir().unwrap();
        touch(&outer.path().join("tasks.md"));
        let repo = outer.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        assert_eq!(find_task_file(&repo.join("src")), None);
    }

    #[test]
    fn outside_a_repository_only_the_start_directory_is_searched() {
        let outer = tempfile::tempdir().unwrap();
        touch(&outer.path().join("tasks.md"));
        let inner = outer.path().join("inner");
        fs::create_dir_all(&inner).unwrap();
        assert_eq!(find_task_file(&inner), None);
    }
}
