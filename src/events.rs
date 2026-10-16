//! Progress reporting for task runs. The runner reports what happens through
//! a [`RunObserver`], so the same engine drives the plain terminal output and
//! the TUI's progress/log panes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use colored::Colorize;

/// How task output is presented on the terminal (`--output`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Raw output when sequential, prefixed lines when concurrent.
    #[default]
    Auto,
    /// Raw lines, even for concurrent tasks.
    Interleaved,
    /// Every line tagged with its task name.
    Prefixed,
    /// One block per task, printed when it finishes.
    Group,
}

impl std::str::FromStr for OutputMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(OutputMode::Auto),
            "interleaved" => Ok(OutputMode::Interleaved),
            "prefixed" => Ok(OutputMode::Prefixed),
            "group" => Ok(OutputMode::Group),
            other => Err(format!(
                "unknown output mode '{}' (interleaved, prefixed, group)",
                other
            )),
        }
    }
}

/// Which stream a line of task output came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Lifecycle state of a single task within a run.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    /// The task is starting. `dependency` is false for the requested task.
    Started {
        dependency: bool,
    },
    /// A failed attempt will be retried (`attempt` of `max` attempts failed).
    Retrying {
        attempt: u32,
        max: u32,
        delay: Duration,
    },
    Succeeded {
        elapsed: Duration,
    },
    /// Not run: `if` was false, or up to date per `sources`/`outputs`.
    Skipped {
        reason: String,
    },
    Failed {
        error: String,
    },
    /// Not run because the run was aborted or a prerequisite failed.
    Cancelled {
        reason: String,
    },
}

/// Receives progress for a run. Called from multiple threads in parallel runs.
pub trait RunObserver: Send + Sync {
    /// The full set of tasks (in execution order) this run will consider.
    fn plan(&self, _tasks: &[String]) {}
    /// Declared dependencies of every planned task as `(task, depends_on)`,
    /// for observers that draw the dependency graph.
    fn graph(&self, _edges: &[(String, Vec<String>)]) {}
    /// One line of output from a task (without the trailing newline).
    fn output(&self, task: &str, stream: Stream, line: &str);
    fn status(&self, task: &str, status: &TaskStatus);
}

/// Prints progress to the terminal.
///
/// In `prefixed` mode (parallel runs) every output line is tagged with the
/// task name and completion is reported, so interleaved output stays legible.
pub struct ConsoleObserver {
    prefixed: bool,
    width: usize,
    /// Hold output until the task finishes (`--output group`).
    group: bool,
    held: Mutex<HashMap<String, Vec<(Stream, String)>>>,
}

impl ConsoleObserver {
    pub fn new() -> Self {
        Self {
            prefixed: false,
            width: 0,
            group: false,
            held: Mutex::default(),
        }
    }

    pub fn prefixed() -> Self {
        Self {
            prefixed: true,
            ..Self::new()
        }
    }

    /// Print each task's output as one block when it finishes.
    pub fn grouped() -> Self {
        Self {
            group: true,
            ..Self::new()
        }
    }

    /// Print the held output of `task` under its name.
    fn flush(&self, task: &str, held: &mut HashMap<String, Vec<(Stream, String)>>) {
        println!("{} {}", "▶".cyan().bold(), task.bold());
        for (stream, line) in held.remove(task).unwrap_or_default() {
            match stream {
                Stream::Stdout => println!("{}", line),
                Stream::Stderr => eprintln!("{}", line),
            }
        }
    }
}

impl Default for ConsoleObserver {
    fn default() -> Self {
        Self::new()
    }
}

/// True for the label of one loop iteration or argument call, `task[item]`.
fn is_iteration(label: &str) -> bool {
    label.ends_with(']') && label.contains('[')
}

/// Stable per-task color so a task keeps its color across lines.
fn tag(name: &str, width: usize) -> String {
    let palette = [
        colored::Color::Cyan,
        colored::Color::Magenta,
        colored::Color::Yellow,
        colored::Color::Green,
        colored::Color::Blue,
        colored::Color::BrightRed,
    ];
    let idx = name.bytes().fold(0usize, |a, b| a.wrapping_add(b as usize)) % palette.len();
    format!("{:<width$}", name, width = width)
        .color(palette[idx])
        .to_string()
}

impl RunObserver for ConsoleObserver {
    fn output(&self, task: &str, stream: Stream, line: &str) {
        if self.group {
            let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
            held.entry(task.to_string())
                .or_default()
                .push((stream, line.to_string()));
            return;
        }
        let line = if self.prefixed {
            format!("{} {} {}", tag(task, self.width), "│".dimmed(), line)
        } else {
            line.to_string()
        };
        match stream {
            Stream::Stdout => println!("{}", line),
            Stream::Stderr => eprintln!("{}", line),
        }
    }

    fn status(&self, task: &str, status: &TaskStatus) {
        if self.group {
            self.status_grouped(task, status);
            return;
        }
        match status {
            TaskStatus::Started { dependency } => {
                if self.prefixed {
                    println!("{} {}", "▶".cyan().bold(), task.bold());
                } else if *dependency {
                    // The requested task's header is printed before the run.
                    println!("{}", format!("↳ {} (dependency)", task).dimmed());
                } else if is_iteration(task) {
                    // Loop items share the task's header; mark each start.
                    println!("{}", format!("↳ {}", task).dimmed());
                }
            }
            TaskStatus::Retrying {
                attempt,
                max,
                delay,
            } => {
                eprintln!(
                    "{}",
                    format!(
                        "↻ {} failed (attempt {}/{}), retrying in {:.1}s",
                        task,
                        attempt,
                        max,
                        delay.as_secs_f64()
                    )
                    .yellow()
                );
            }
            TaskStatus::Succeeded { elapsed } => {
                if self.prefixed {
                    println!(
                        "{} {} {}",
                        "✔".green().bold(),
                        task.bold(),
                        format!("({:.1}s)", elapsed.as_secs_f64()).dimmed()
                    );
                }
            }
            TaskStatus::Skipped { reason } => {
                println!("{}", format!("⊘ {} skipped ({})", task, reason).dimmed());
            }
            TaskStatus::Failed { error } => {
                if self.prefixed {
                    eprintln!("{} {}: {}", "✘".red().bold(), task.bold(), error);
                }
            }
            TaskStatus::Cancelled { reason } => {
                eprintln!("{}", format!("⊘ {} not run ({})", task, reason).dimmed());
            }
        }
    }
}

impl ConsoleObserver {
    /// Status reporting for `--output group`.
    fn status_grouped(&self, task: &str, status: &TaskStatus) {
        // Holding the lock while printing keeps blocks from interleaving.
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        match status {
            TaskStatus::Started { .. } => {}
            TaskStatus::Retrying {
                attempt,
                max,
                delay,
            } => {
                eprintln!(
                    "{}",
                    format!(
                        "↻ {} failed (attempt {}/{}), retrying in {:.1}s",
                        task,
                        attempt,
                        max,
                        delay.as_secs_f64()
                    )
                    .yellow()
                );
            }
            TaskStatus::Succeeded { elapsed } => {
                self.flush(task, &mut held);
                println!(
                    "{} {} {}",
                    "✔".green().bold(),
                    task.bold(),
                    format!("({:.1}s)", elapsed.as_secs_f64()).dimmed()
                );
            }
            TaskStatus::Failed { error } => {
                self.flush(task, &mut held);
                eprintln!("{} {}: {}", "✘".red().bold(), task.bold(), error);
            }
            TaskStatus::Skipped { reason } => {
                held.remove(task);
                println!("{}", format!("⊘ {} skipped ({})", task, reason).dimmed());
            }
            TaskStatus::Cancelled { reason } => {
                held.remove(task);
                eprintln!("{}", format!("⊘ {} not run ({})", task, reason).dimmed());
            }
        }
    }
}

/// An observer that discards everything (used for plain library calls).
pub struct NullObserver;

impl RunObserver for NullObserver {
    fn output(&self, _task: &str, _stream: Stream, _line: &str) {}
    fn status(&self, _task: &str, _status: &TaskStatus) {}
}

impl ConsoleObserver {
    /// Prefixed observer sized for the given task names.
    pub fn prefixed_for(tasks: &[String]) -> Self {
        Self {
            prefixed: true,
            width: tasks.iter().map(|t| t.chars().count()).max().unwrap_or(0),
            ..Self::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// Remove ANSI escape sequences (`ESC [ ... m`).
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[rstest]
    #[case("build", 5, "build")]
    #[case("build", 8, "build   ")]
    #[case("build", 0, "build")]
    #[case("build", 2, "build")]
    #[case("", 3, "   ")]
    fn tag_pads_to_the_requested_width(
        #[case] name: &str,
        #[case] width: usize,
        #[case] expected: &str,
    ) {
        assert_eq!(strip_ansi(&tag(name, width)), expected);
    }

    #[test]
    fn tag_color_is_stable_per_name() {
        assert_eq!(tag("lint", 6), tag("lint", 6));
    }

    #[test]
    fn prefixed_for_uses_the_longest_name_in_characters() {
        let o = ConsoleObserver::prefixed_for(&["a".into(), "日本語".into(), "bb".into()]);
        assert_eq!(o.width, 3);
        assert!(o.prefixed);
        assert_eq!(ConsoleObserver::prefixed_for(&[]).width, 0);
    }

    #[test]
    fn constructors_set_the_prefix_mode() {
        assert!(!ConsoleObserver::new().prefixed);
        assert!(!ConsoleObserver::default().prefixed);
        assert!(ConsoleObserver::prefixed().prefixed);
    }

    #[test]
    fn every_status_can_be_reported_without_panicking() {
        let statuses = [
            TaskStatus::Started { dependency: true },
            TaskStatus::Started { dependency: false },
            TaskStatus::Retrying {
                attempt: 1,
                max: 3,
                delay: Duration::from_millis(1500),
            },
            TaskStatus::Succeeded {
                elapsed: Duration::from_millis(12),
            },
            TaskStatus::Skipped {
                reason: "if: false".into(),
            },
            TaskStatus::Failed {
                error: "boom".into(),
            },
            TaskStatus::Cancelled {
                reason: "aborted".into(),
            },
        ];
        for observer in [
            ConsoleObserver::new(),
            ConsoleObserver::prefixed_for(&["t".into()]),
        ] {
            for status in &statuses {
                observer.status("t", status);
            }
        }
        NullObserver.status("t", &statuses[0]);
        NullObserver.output("t", Stream::Stdout, "ignored");
        NullObserver.plan(&["t".into()]);
        NullObserver.graph(&[("t".into(), vec![])]);
    }

    #[rstest]
    #[case("auto", OutputMode::Auto)]
    #[case("Interleaved", OutputMode::Interleaved)]
    #[case("PREFIXED", OutputMode::Prefixed)]
    #[case("group", OutputMode::Group)]
    fn parses_output_modes(#[case] raw: &str, #[case] expected: OutputMode) {
        assert_eq!(raw.parse::<OutputMode>().unwrap(), expected);
    }

    #[test]
    fn rejects_unknown_output_modes() {
        assert!("loud".parse::<OutputMode>().unwrap_err().contains("loud"));
    }

    #[test]
    fn grouped_output_is_held_until_the_task_finishes() {
        let o = ConsoleObserver::grouped();
        o.output("t", Stream::Stdout, "line");
        o.output("t", Stream::Stderr, "err");
        o.output("u", Stream::Stdout, "other");
        assert_eq!(o.held.lock().unwrap()["t"].len(), 2);
        o.status(
            "t",
            &TaskStatus::Succeeded {
                elapsed: Duration::from_millis(1),
            },
        );
        let held = o.held.lock().unwrap();
        assert!(!held.contains_key("t"));
        assert_eq!(held["u"].len(), 1);
    }

    proptest! {
        #[test]
        fn tag_never_truncates_and_pads_with_spaces(name in "[ -~]{0,12}", width in 0usize..20) {
            let plain = strip_ansi(&tag(&name, width));
            prop_assert!(plain.starts_with(&name));
            prop_assert_eq!(plain.chars().count(), name.chars().count().max(width));
            prop_assert!(plain[name.len()..].chars().all(|c| c == ' '));
        }
    }
}
