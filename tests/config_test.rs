//! `config`: execution modes, runtime overrides, TOML loading.

use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;

use mq_task::config::{Config, ExecutionMode, RuntimeConfig};
use mq_task::error::Error;
use proptest::prelude::*;
use rstest::rstest;

fn overrides(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[rstest]
#[case("stdin", ExecutionMode::Stdin)]
#[case("file", ExecutionMode::File)]
#[case("arg", ExecutionMode::Arg)]
#[case("STDIN", ExecutionMode::Stdin)]
#[case("File", ExecutionMode::File)]
#[case("ARG", ExecutionMode::Arg)]
fn execution_mode_from_str(#[case] raw: &str, #[case] expected: ExecutionMode) {
    assert_eq!(ExecutionMode::try_from(raw).unwrap(), expected);
}

#[rstest]
#[case("")]
#[case("args")]
#[case("stdin ")]
#[case("pipe")]
fn execution_mode_rejects_unknown(#[case] raw: &str) {
    let err = ExecutionMode::try_from(raw).unwrap_err();
    assert!(matches!(err, Error::Config(_)));
    let msg = err.to_string();
    assert!(msg.contains("Invalid execution mode"), "{msg}");
    assert!(msg.contains("stdin, file, arg"), "{msg}");
}

#[test]
fn execution_mode_defaults_to_stdin() {
    assert_eq!(ExecutionMode::default(), ExecutionMode::Stdin);
}

#[rstest]
#[case("bash", "bash", ExecutionMode::File)]
#[case("sh", "sh", ExecutionMode::File)]
#[case("python", "python3", ExecutionMode::File)]
#[case("ruby", "ruby", ExecutionMode::File)]
#[case("node", "node", ExecutionMode::File)]
#[case("javascript", "node", ExecutionMode::File)]
#[case("js", "node", ExecutionMode::File)]
#[case("php", "php", ExecutionMode::File)]
#[case("perl", "perl", ExecutionMode::File)]
#[case("go", "go run", ExecutionMode::File)]
#[case("golang", "go run", ExecutionMode::File)]
#[case("jq", "jq", ExecutionMode::Arg)]
#[case("mq", "mq", ExecutionMode::Arg)]
fn default_runtimes(#[case] lang: &str, #[case] command: &str, #[case] mode: ExecutionMode) {
    let config = Config::default();
    assert!(config.has_runtime(lang));
    assert_eq!(config.get_runtime(lang), Some(command));
    assert_eq!(config.get_execution_mode(lang), mode);
}

#[test]
fn unknown_languages_have_no_runtime_and_a_default_mode() {
    let config = Config::default();
    assert!(!config.has_runtime("cobol"));
    assert_eq!(config.get_runtime("cobol"), None);
    assert_eq!(config.get_execution_mode("cobol"), ExecutionMode::Stdin);
    assert_eq!(config.default_task, None);
}

#[rstest]
#[case::simple(
    "[runtimes]\npy = \"python3\"\n",
    "py",
    "python3",
    ExecutionMode::Stdin
)]
#[case::detailed_file(
    "[runtimes.go]\ncommand = \"go run\"\nexecution_mode = \"file\"\n",
    "go",
    "go run",
    ExecutionMode::File
)]
#[case::detailed_arg(
    "[runtimes.q]\ncommand = \"mq\"\nexecution_mode = \"arg\"\n",
    "q",
    "mq",
    ExecutionMode::Arg
)]
#[case::detailed_defaults_to_stdin(
    "[runtimes.x]\ncommand = \"x\"\n",
    "x",
    "x",
    ExecutionMode::Stdin
)]
fn toml_runtimes(
    #[case] toml: &str,
    #[case] lang: &str,
    #[case] command: &str,
    #[case] mode: ExecutionMode,
) {
    let config: Config = toml::from_str(toml).unwrap();
    assert_eq!(config.get_runtime(lang), Some(command));
    assert_eq!(config.get_execution_mode(lang), mode);
}

#[test]
fn toml_without_runtimes_falls_back_to_defaults() {
    let config: Config = toml::from_str("default_task = \"build\"").unwrap();
    assert_eq!(config.default_task.as_deref(), Some("build"));
    assert_eq!(config.get_runtime("bash"), Some("bash"));
}

#[test]
fn toml_runtimes_replace_the_defaults_wholesale() {
    let config: Config = toml::from_str("[runtimes]\nonly = \"x\"\n").unwrap();
    assert!(config.has_runtime("only"));
    assert!(!config.has_runtime("bash"));
}

#[rstest]
#[case::bad_mode("[runtimes.x]\ncommand = \"x\"\nexecution_mode = \"nope\"\n")]
#[case::missing_command("[runtimes.x]\nexecution_mode = \"file\"\n")]
#[case::wrong_type("[runtimes]\nx = 1\n")]
#[case::not_toml("this is = = not toml")]
fn toml_errors(#[case] toml: &str) {
    assert!(toml::from_str::<Config>(toml).is_err());
}

#[test]
fn serialization_round_trips() {
    let config = Config {
        default_task: Some("ci".into()),
        ..Default::default()
    };
    let text = toml::to_string(&config).unwrap();
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(back.default_task, config.default_task);
    assert_eq!(back.runtimes, config.runtimes);
}

#[test]
fn from_file_reads_a_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mq_task.toml");
    fs::write(&path, "default_task = \"x\"\n[runtimes]\nfoo = \"bar\"\n").unwrap();
    let config = Config::from_file(&path).unwrap();
    assert_eq!(config.default_task.as_deref(), Some("x"));
    assert_eq!(config.get_runtime("foo"), Some("bar"));
}

#[test]
fn from_file_reports_io_and_parse_errors() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        Config::from_file(dir.path().join("missing.toml")),
        Err(Error::Io(_))
    ));
    let bad = dir.path().join("bad.toml");
    fs::write(&bad, "= nope").unwrap();
    assert!(matches!(Config::from_file(&bad), Err(Error::TomlParse(_))));
}

#[rstest]
#[case(RuntimeConfig::Simple("python3".into()), "python3", ExecutionMode::Stdin)]
#[case(
    RuntimeConfig::Detailed { command: "go run".into(), execution_mode: ExecutionMode::File },
    "go run",
    ExecutionMode::File
)]
#[case(
    RuntimeConfig::Detailed { command: "mq".into(), execution_mode: ExecutionMode::Arg },
    "mq",
    ExecutionMode::Arg
)]
fn runtime_config_accessors(
    #[case] rc: RuntimeConfig,
    #[case] command: &str,
    #[case] mode: ExecutionMode,
) {
    assert_eq!(rc.command(), command);
    assert_eq!(rc.execution_mode(), mode);
}

#[rstest]
#[case::replace_default("python:python3.11", "python", "python3.11")]
#[case::new_language("lua:lua5.4", "lua", "lua5.4")]
#[case::command_with_args("python:python3 -u", "python", "python3 -u")]
#[case::command_containing_colons("sh:env A=b:c sh", "sh", "env A=b:c sh")]
#[case::empty_command("x:", "x", "")]
fn applies_runtime_overrides(#[case] raw: &str, #[case] lang: &str, #[case] command: &str) {
    let mut config = Config::default();
    config
        .apply_runtime_overrides(&overrides(&[raw]), None)
        .unwrap();
    assert_eq!(config.get_runtime(lang), Some(command));
}

#[rstest]
#[case("python")]
#[case("")]
#[case("no-colon-here")]
fn rejects_malformed_overrides(#[case] raw: &str) {
    let mut config = Config::default();
    let err = config
        .apply_runtime_overrides(&overrides(&[raw]), None)
        .unwrap_err();
    assert!(matches!(err, Error::Config(_)));
    assert!(err.to_string().contains("lang:command"), "{err}");
}

#[test]
fn override_without_a_mode_is_a_simple_runtime() {
    let mut config = Config::default();
    config
        .apply_runtime_overrides(&overrides(&["bash:zsh"]), None)
        .unwrap();
    // Replacing a default drops its execution mode: back to stdin.
    assert_eq!(config.get_execution_mode("bash"), ExecutionMode::Stdin);
    assert_eq!(config.runtimes["bash"], RuntimeConfig::Simple("zsh".into()));
}

#[rstest]
#[case(ExecutionMode::Stdin)]
#[case(ExecutionMode::File)]
#[case(ExecutionMode::Arg)]
fn override_mode_applies_to_every_override(#[case] mode: ExecutionMode) {
    let mut config = Config::default();
    config
        .apply_runtime_overrides(&overrides(&["a:one", "b:two"]), Some(mode.clone()))
        .unwrap();
    for (lang, cmd) in [("a", "one"), ("b", "two")] {
        assert_eq!(config.get_runtime(lang), Some(cmd));
        assert_eq!(config.get_execution_mode(lang), mode);
    }
}

#[test]
fn later_overrides_win_and_other_runtimes_are_untouched() {
    let mut config = Config::default();
    config
        .apply_runtime_overrides(&overrides(&["x:first", "x:second"]), None)
        .unwrap();
    assert_eq!(config.get_runtime("x"), Some("second"));
    assert_eq!(config.get_runtime("python"), Some("python3"));
}

#[test]
fn an_invalid_override_after_valid_ones_still_errors() {
    let mut config = Config::default();
    let result = config.apply_runtime_overrides(&overrides(&["a:ok", "broken"]), None);
    assert!(result.is_err());
}

proptest! {
    #[test]
    fn override_round_trips_lang_and_command(
        lang in "[a-z][a-z0-9_]{0,10}",
        command in "[ -~]{0,30}",
        mode in prop::option::of(prop_oneof![
            Just(ExecutionMode::Stdin), Just(ExecutionMode::File), Just(ExecutionMode::Arg)
        ]),
    ) {
        let mut config = Config { runtimes: HashMap::new(), default_task: None };
        config
            .apply_runtime_overrides(&[format!("{lang}:{command}")], mode.clone())
            .unwrap();
        // Only the first colon splits, so any command text survives intact.
        prop_assert_eq!(config.get_runtime(&lang), Some(command.as_str()));
        prop_assert_eq!(config.get_execution_mode(&lang), mode.unwrap_or_default());
    }

    #[test]
    fn an_override_without_a_colon_is_always_rejected(raw in "[^:]{0,20}") {
        let mut config = Config::default();
        prop_assert!(config.apply_runtime_overrides(&[raw], None).is_err());
    }
}

fn only(lang: &str, command: &str) -> Config {
    Config {
        runtimes: HashMap::from([(lang.to_string(), RuntimeConfig::Simple(command.into()))]),
        default_task: None,
    }
}

#[test]
fn validate_accepts_runtimes_on_path() {
    only("sh", "sh").validate_runtimes().unwrap();
}

#[test]
fn validate_checks_only_the_binary_not_its_arguments() {
    only("sh", "sh -c --definitely-not-a-flag")
        .validate_runtimes()
        .unwrap();
}

#[test]
fn validate_names_the_missing_runtime() {
    let err = only("zz", "mq-task-no-such-binary-xyz")
        .validate_runtimes()
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("mq-task-no-such-binary-xyz"), "{msg}");
    assert!(msg.contains("'zz'"), "{msg}");
}

#[test]
fn validate_accepts_an_empty_runtime_table() {
    Config {
        runtimes: HashMap::new(),
        default_task: None,
    }
    .validate_runtimes()
    .unwrap();
}
