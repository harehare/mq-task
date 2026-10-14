//! `Error`: messages and conversions.

use mq_task::Error;
use rstest::rstest;

#[rstest]
#[case(Error::Markdown("m".into()), "Markdown error: m")]
#[case(Error::Query("q".into()), "Query error: q")]
#[case(Error::Execution("e".into()), "Execution error: e")]
#[case(Error::Config("c".into()), "Config error: c")]
#[case(Error::SectionNotFound("build".into()), "Section not found: build")]
#[case(Error::NoCommands("agg".into()), "Task 'agg' has no commands to run")]
#[case(Error::RuntimeNotFound("cobol".into()), "Runtime not found for language: cobol")]
#[case(
    Error::CircularDependency("a".into()),
    "Circular dependency detected involving task: a"
)]
#[case(Error::ExecutionFailed(3), "Task exited with status code 3")]
#[case(
    Error::MissingParameter("env".into(), "deploy".into()),
    "Missing required parameter 'env' for task 'deploy'"
)]
#[case(
    Error::InvalidEnv("NOPE".into()),
    "Invalid environment variable 'NOPE'. Expected format: 'KEY=VALUE'"
)]
#[case(Error::Timeout("slow".into()), "Task 'slow' timed out")]
#[case(Error::Cancelled, "Task was cancelled")]
#[case(Error::Invalid("bad".into()), "Invalid task file: bad")]
fn error_messages(#[case] error: Error, #[case] expected: &str) {
    assert_eq!(error.to_string(), expected);
}

#[test]
fn io_errors_convert() {
    let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let err: Error = io.into();
    assert!(matches!(err, Error::Io(_)));
    assert_eq!(err.to_string(), "IO error: gone");
}

#[test]
fn toml_errors_convert() {
    let parse = toml::from_str::<toml::Table>("= bad").unwrap_err();
    let err: Error = parse.into();
    assert!(matches!(err, Error::TomlParse(_)));
    assert!(err.to_string().starts_with("TOML parse error: "));
}
