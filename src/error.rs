//! Error types for mq_task

use thiserror::Error;

/// Result type for mq_task operations
pub type Result<T> = std::result::Result<T, Error>;

/// Error types for mq_task operations
#[derive(Error, Debug)]
pub enum Error {
    /// Error reading or parsing Markdown file
    #[error("Markdown error: {0}")]
    Markdown(String),

    /// Error executing mq query
    #[error("Query error: {0}")]
    Query(String),

    /// Error executing code block
    #[error("Execution error: {0}")]
    Execution(String),

    /// Configuration error
    #[error("Config error: {0}")]
    Config(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML parsing error
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// No task file was given and none could be found
    #[error(
        "No task file found: looked for tasks.md and README.md in {0} and its parent directories up to the repository root. Use -f/--file or set MQ_TASK_FILE"
    )]
    NoTaskFile(String),

    /// Section not found
    #[error("Section not found: {0}")]
    SectionNotFound(String),

    /// Section has neither code blocks nor dependencies
    #[error("Task '{0}' has no commands to run")]
    NoCommands(String),

    /// Runtime not found
    #[error("Runtime not found for language: {0}")]
    RuntimeNotFound(String),

    /// Circular dependency detected
    #[error("Circular dependency detected involving task: {0}")]
    CircularDependency(String),

    /// Task process exited with a non-zero status
    #[error("Task exited with status code {0}")]
    ExecutionFailed(i32),

    /// A required task parameter was not provided
    #[error("Missing required parameter '{0}' for task '{1}'")]
    MissingParameter(String, String),

    /// A `name=value` argument names a parameter the task does not declare
    #[error("Unknown parameter '{0}' for task '{1}'{2}")]
    UnknownParameter(String, String, String),

    /// A parameter was given a value outside its `choices`
    #[error("Invalid value '{1}' for parameter '{0}' of task '{2}' ({3})")]
    InvalidParameter(String, String, String, String),

    /// A `--env` value was not in `KEY=VALUE` format
    #[error("Invalid environment variable '{0}'. Expected format: 'KEY=VALUE'")]
    InvalidEnv(String),

    /// A task ran longer than its `timeout`
    #[error("Task '{0}' timed out")]
    Timeout(String),

    /// Variables a task `requires` are not set
    #[error("Task '{1}' requires {0} to be set")]
    MissingVariable(String, String),

    /// A `vars` entry could not be resolved
    #[error("Variable '{0}': {1}")]
    Variable(String, String),

    /// A task would run (`--status` found it out of date)
    #[error("out of date ({0})")]
    OutOfDate(String),

    /// `mq-task check` found problems (already printed)
    #[error("check failed")]
    CheckFailed,

    /// A task's `prompt` was declined
    #[error("Task '{0}' was not confirmed")]
    Declined(String),

    /// A task's `prompt` cannot be asked in this run
    #[error("Task '{0}' needs confirmation; pass --yes to run it without prompting")]
    ConfirmationRequired(String),

    /// A task was aborted because another task failed
    #[error("Task was cancelled")]
    Cancelled,

    /// Invalid task file contents (imports, durations, ...)
    #[error("Invalid task file: {0}")]
    Invalid(String),

    /// A failure inside a task that does not name the task itself
    #[error("{source} (in task '{task}')")]
    TaskFailed {
        task: String,
        #[source]
        source: Box<Error>,
    },
}

impl Error {
    /// The error without any task context.
    pub fn root(&self) -> &Error {
        match self {
            Error::TaskFailed { source, .. } => source.root(),
            other => other,
        }
    }

    /// Process exit code for this error: the task's own code, 124 for a
    /// timeout (like coreutils `timeout`), else 1.
    pub fn exit_code(&self) -> i32 {
        match self.root() {
            Error::ExecutionFailed(code) => *code,
            Error::Timeout(_) => 124,
            _ => 1,
        }
    }

    /// Attach the name of the failing task to errors that lack it.
    pub fn in_task(self, task: &str) -> Error {
        match self {
            Error::ExecutionFailed(_)
            | Error::Execution(_)
            | Error::RuntimeNotFound(_)
            | Error::Io(_) => Error::TaskFailed {
                task: task.to_string(),
                source: Box::new(self),
            },
            other => other,
        }
    }
}
