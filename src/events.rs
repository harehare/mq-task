//! Progress reporting for task runs. The runner reports what happens through
//! a [`RunObserver`], so the same engine drives the plain terminal output and
//! the TUI's progress/log panes.

use std::time::Duration;

use colored::Colorize;

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
}

impl ConsoleObserver {
    pub fn new() -> Self {
        Self {
            prefixed: false,
            width: 0,
        }
    }

    pub fn prefixed() -> Self {
        Self {
            prefixed: true,
            width: 0,
        }
    }
}

impl Default for ConsoleObserver {
    fn default() -> Self {
        Self::new()
    }
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
        match status {
            TaskStatus::Started { dependency } => {
                if self.prefixed {
                    println!("{} {}", "▶".cyan().bold(), task.bold());
                } else if *dependency {
                    // The requested task's header is printed before the run.
                    println!("{}", format!("↳ {} (dependency)", task).dimmed());
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
