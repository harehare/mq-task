//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::sync::Mutex;

use mq_task::events::{RunObserver, Stream, TaskStatus};
use mq_task::runner::{CodeBlock, Section};

/// A section with the given title and declared dependencies.
pub fn sec(title: &str, depends: &[&str]) -> Section {
    Section {
        title: title.to_string(),
        depends: depends.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// A runnable section: one `bash` block holding `code`.
pub fn bash(title: &str, depends: &[&str], code: &str) -> Section {
    Section {
        codes: vec![CodeBlock {
            lang: "bash".into(),
            code: code.into(),
        }],
        ..sec(title, depends)
    }
}

/// Everything an observer was told, in arrival order.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Plan(Vec<String>),
    Output(String, Stream, String),
    Status(String, TaskStatus),
}

#[derive(Default)]
pub struct Recorder {
    events: Mutex<Vec<Event>>,
}

impl Recorder {
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    /// Statuses reported for `task`, in order.
    pub fn statuses(&self, task: &str) -> Vec<TaskStatus> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Status(t, s) if t == task => Some(s),
                _ => None,
            })
            .collect()
    }

    /// Stdout lines emitted by `task`.
    pub fn stdout(&self, task: &str) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Output(t, Stream::Stdout, l) if t == task => Some(l),
                _ => None,
            })
            .collect()
    }

    /// Position of the first event of `task` matching `pred`.
    pub fn position(&self, task: &str, pred: impl Fn(&TaskStatus) -> bool) -> Option<usize> {
        self.events().iter().position(|e| match e {
            Event::Status(t, s) => t == task && pred(s),
            _ => false,
        })
    }
}

impl RunObserver for Recorder {
    fn plan(&self, tasks: &[String]) {
        self.events
            .lock()
            .unwrap()
            .push(Event::Plan(tasks.to_vec()));
    }

    fn output(&self, task: &str, stream: Stream, line: &str) {
        self.events
            .lock()
            .unwrap()
            .push(Event::Output(task.into(), stream, line.into()));
    }

    fn status(&self, task: &str, status: &TaskStatus) {
        self.events
            .lock()
            .unwrap()
            .push(Event::Status(task.into(), status.clone()));
    }
}
