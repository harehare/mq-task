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

#[test]
fn in_task_names_the_task_for_failures_that_do_not() {
    let err = Error::ExecutionFailed(3).in_task("build");
    assert_eq!(
        err.to_string(),
        "Task exited with status code 3 (in task 'build')"
    );
    assert!(matches!(err.root(), Error::ExecutionFailed(3)));
    assert_eq!(err.exit_code(), 3);

    for e in [
        Error::Execution("boom".into()),
        Error::RuntimeNotFound("cobol".into()),
        Error::Io(std::io::Error::other("disk")),
    ] {
        assert!(matches!(e.in_task("t"), Error::TaskFailed { .. }));
    }
}

#[test]
fn errors_that_already_name_the_task_are_not_wrapped() {
    for e in [
        Error::Timeout("t".into()),
        Error::MissingParameter("p".into(), "t".into()),
        Error::MissingVariable("X".into(), "t".into()),
        Error::Declined("t".into()),
        Error::ConfirmationRequired("t".into()),
        Error::Cancelled,
        Error::Invalid("x".into()),
    ] {
        let text = e.to_string();
        let wrapped = e.in_task("other");
        assert_eq!(wrapped.to_string(), text);
        assert!(!matches!(wrapped, Error::TaskFailed { .. }));
    }
}

#[test]
fn exit_codes_follow_the_root_error() {
    assert_eq!(Error::ExecutionFailed(7).exit_code(), 7);
    assert_eq!(Error::Timeout("t".into()).exit_code(), 124);
    assert_eq!(Error::Cancelled.exit_code(), 1);
    assert_eq!(Error::Invalid("x".into()).exit_code(), 1);
    let nested = Error::TaskFailed {
        task: "outer".into(),
        source: Box::new(Error::ExecutionFailed(9).in_task("inner")),
    };
    assert_eq!(nested.exit_code(), 9);
    assert!(matches!(nested.root(), Error::ExecutionFailed(9)));
}
