//! Progress reporting for task runs. The runner reports what happens through
//! a [`RunObserver`], so the same engine drives the plain terminal output and
//! the TUI's progress/log panes.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
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
    held: Mutex<HashMap<String, Spool>>,
}

/// Output of one task held for `--output group`. Lines go to an anonymous
/// temporary file, so a verbose task costs disk rather than memory, and the
/// file disappears when the spool is dropped, however the task ends.
struct Spool {
    writer: BufWriter<File>,
}

impl Spool {
    fn new() -> io::Result<Self> {
        Ok(Self {
            writer: BufWriter::new(tempfile::tempfile()?),
        })
    }

    /// Append one line as a stream tag, a length and the bytes.
    fn push(&mut self, stream: Stream, line: &str) -> io::Result<()> {
        let tag = match stream {
            Stream::Stdout => 0u8,
            Stream::Stderr => 1u8,
        };
        let len = u32::try_from(line.len()).map_err(io::Error::other)?;
        self.writer.write_all(&[tag])?;
        self.writer.write_all(&len.to_le_bytes())?;
        self.writer.write_all(line.as_bytes())
    }

    /// Hand every held line to `each`, in order.
    fn drain(self, mut each: impl FnMut(Stream, &str)) -> io::Result<()> {
        let mut file = self.writer.into_inner().map_err(|e| e.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::new(file);
        let mut head = [0u8; 5];
        loop {
            match reader.read_exact(&mut head) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            }
            let mut buf =
                vec![0u8; u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize];
            reader.read_exact(&mut buf)?;
            let stream = if head[0] == 0 {
                Stream::Stdout
            } else {
                Stream::Stderr
            };
            each(stream, &String::from_utf8_lossy(&buf));
        }
    }

    /// Print everything held, in order.
    fn replay(self) -> io::Result<()> {
        self.drain(|stream, line| match stream {
            Stream::Stdout => println!("{}", line),
            Stream::Stderr => eprintln!("{}", line),
        })
    }
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
    fn flush(&self, task: &str, held: &mut HashMap<String, Spool>) {
        println!("{} {}", "▶".cyan().bold(), task.bold());
        if let Some(spool) = held.remove(task)
            && let Err(e) = spool.replay()
        {
            eprintln!("{}", format!("{}: output lost ({})", task, e).yellow());
        }
    }
}

impl Default for ConsoleObserver {
    fn default() -> Self {
        Self::new()
    }
}

/// Hold `line` in `task`'s spool, creating the spool on its first line.
fn spool_line(
    held: &mut HashMap<String, Spool>,
    task: &str,
    stream: Stream,
    line: &str,
) -> io::Result<()> {
    if !held.contains_key(task) {
        held.insert(task.to_string(), Spool::new()?);
    }
    match held.get_mut(task) {
        Some(spool) => spool.push(stream, line),
        None => Err(io::Error::other("spool missing")),
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
            if spool_line(&mut held, task, stream, line).is_ok() {
                return;
            }
            // The spool is unusable (e.g. disk full): print the line now
            // rather than lose it.
            match stream {
                Stream::Stdout => println!("{}", line),
                Stream::Stderr => eprintln!("{}", line),
            }
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

/// Passes everything on to `inner` and remembers which task failed first, so
/// the error of a run can name it.
pub struct FailureTracker<'a> {
    inner: &'a dyn RunObserver,
    first: Mutex<Option<String>>,
}

impl<'a> FailureTracker<'a> {
    pub fn new(inner: &'a dyn RunObserver) -> Self {
        Self {
            inner,
            first: Mutex::new(None),
        }
    }

    /// The first task that reported a failure.
    pub fn first_failure(&self) -> Option<String> {
        self.first.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl RunObserver for FailureTracker<'_> {
    fn plan(&self, tasks: &[String]) {
        self.inner.plan(tasks);
    }

    fn graph(&self, edges: &[(String, Vec<String>)]) {
        self.inner.graph(edges);
    }

    fn output(&self, task: &str, stream: Stream, line: &str) {
        self.inner.output(task, stream, line);
    }

    fn status(&self, task: &str, status: &TaskStatus) {
        if matches!(status, TaskStatus::Failed { .. }) {
            self.first
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(|| task.to_string());
        }
        self.inner.status(task, status);
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

    #[test]
    fn the_failure_tracker_remembers_the_first_failed_task() {
        let tracker = FailureTracker::new(&NullObserver);
        assert_eq!(tracker.first_failure(), None);
        tracker.status("ok", &TaskStatus::Started { dependency: false });
        tracker.status("cancelled", &TaskStatus::Cancelled { reason: "x".into() });
        assert_eq!(tracker.first_failure(), None);
        tracker.status("first", &TaskStatus::Failed { error: "a".into() });
        tracker.status("second", &TaskStatus::Failed { error: "b".into() });
        assert_eq!(tracker.first_failure().as_deref(), Some("first"));
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
        assert_eq!(
            drained(o.held.lock().unwrap().remove("t").unwrap()),
            vec![
                (Stream::Stdout, "line".to_string()),
                (Stream::Stderr, "err".to_string())
            ]
        );
        o.output("t", Stream::Stdout, "again");
        o.status(
            "t",
            &TaskStatus::Succeeded {
                elapsed: Duration::from_millis(1),
            },
        );
        let mut held = o.held.lock().unwrap();
        assert!(!held.contains_key("t"));
        assert_eq!(drained(held.remove("u").unwrap()).len(), 1);
    }

    fn drained(spool: Spool) -> Vec<(Stream, String)> {
        let mut lines = Vec::new();
        spool
            .drain(|stream, line| lines.push((stream, line.to_string())))
            .unwrap();
        lines
    }

    #[test]
    fn spool_keeps_order_streams_and_unicode_lines() {
        let mut spool = Spool::new().unwrap();
        let long = "x".repeat(100_000);
        for (stream, line) in [
            (Stream::Stdout, "日本語"),
            (Stream::Stderr, ""),
            (Stream::Stdout, long.as_str()),
        ] {
            spool.push(stream, line).unwrap();
        }
        let lines = drained(spool);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], (Stream::Stdout, "日本語".to_string()));
        assert_eq!(lines[1], (Stream::Stderr, String::new()));
        assert_eq!(lines[2].1.len(), 100_000);
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
