//! `condition::evaluate`: the `if` expression language.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use mq_task::condition::{CondContext, evaluate};
use mq_task::error::{Error, Result};
use proptest::prelude::*;
use rstest::rstest;

const UNSET: &str = "MQ_TASK_SURELY_UNSET_VAR_8f3a";

fn eval_in(expr: &str, env: &[(&str, &str)], dir: Option<&Path>) -> Result<bool> {
    let map: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    evaluate(expr, &CondContext { env: &map, dir })
}

fn eval(expr: &str, env: &[(&str, &str)]) -> Result<bool> {
    eval_in(expr, env, None)
}

fn invalid_message(expr: &str) -> String {
    match eval(expr, &[]) {
        Err(Error::Config(m)) => m,
        other => panic!("expected a config error for {expr:?}, got {other:?}"),
    }
}

#[rstest]
#[case("1", true)]
#[case("true", true)]
#[case("TRUE", true)]
#[case("True", true)]
#[case("yes", true)]
#[case("on", true)]
#[case("2", true)]
#[case("00", true)]
#[case(" ", true)]
#[case("0", false)]
#[case("false", false)]
#[case("FALSE", false)]
#[case("False", false)]
#[case("", false)]
fn flag_truthiness(#[case] value: &str, #[case] expected: bool) {
    assert_eq!(eval(r#"flag("V")"#, &[("V", value)]).unwrap(), expected);
}

#[test]
fn flag_is_false_when_unset() {
    assert!(!eval(&format!(r#"flag("{UNSET}")"#), &[]).unwrap());
}

#[test]
fn flag_falls_back_to_the_process_environment() {
    // PATH is set in any sane test environment and is neither "", "0" nor "false".
    assert!(eval(r#"flag("PATH")"#, &[]).unwrap());
    // ...and the task environment shadows it.
    assert!(!eval(r#"flag("PATH")"#, &[("PATH", "0")]).unwrap());
}

#[rstest]
#[case(r#"getenv("R") == "eu""#, "eu", true)]
#[case(r#"getenv("R") == "eu""#, "us", false)]
#[case(r#"getenv("R") != "eu""#, "us", true)]
#[case(r#"getenv("R") != "eu""#, "eu", false)]
#[case(r#"getenv("R") == """#, "", true)]
#[case(r#"getenv("R") == "EU""#, "eu", false)]
#[case(r#"getenv("R") == "a b""#, "a b", true)]
fn getenv_comparisons(#[case] expr: &str, #[case] value: &str, #[case] expected: bool) {
    assert_eq!(eval(expr, &[("R", value)]).unwrap(), expected);
}

#[test]
fn getenv_of_an_unset_variable_is_none() {
    let expr = format!(r#"getenv("{UNSET}") == "x""#);
    assert!(!eval(&expr, &[]).unwrap());
    let expr = format!(r#"getenv("{UNSET}") != "x""#);
    assert!(eval(&expr, &[]).unwrap());
}

#[test]
fn task_environment_shadows_the_process_environment() {
    assert!(eval(r#"getenv("PATH") == "mine""#, &[("PATH", "mine")]).unwrap());
}

#[test]
fn os_reports_the_host_os() {
    let expr = format!(r#"os() == "{}""#, std::env::consts::OS);
    assert!(eval(&expr, &[]).unwrap());
    assert!(!eval(r#"os() == "plan9""#, &[]).unwrap());
}

#[rstest]
#[case("sh", true)]
#[case("mq-task-no-such-command-xyz", false)]
#[case("", false)]
fn which_checks_path(#[case] cmd: &str, #[case] expected: bool) {
    assert_eq!(eval(&format!(r#"which("{cmd}")"#), &[]).unwrap(), expected);
}

#[test]
fn exists_resolves_relative_to_the_given_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("present.txt"), "x").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub").join("deep.txt"), "x").unwrap();

    let d = Some(dir.path());
    assert!(eval_in(r#"exists("present.txt")"#, &[], d).unwrap());
    assert!(eval_in(r#"exists("sub")"#, &[], d).unwrap());
    assert!(eval_in(r#"exists("sub/deep.txt")"#, &[], d).unwrap());
    assert!(!eval_in(r#"exists("absent.txt")"#, &[], d).unwrap());
    assert!(!eval_in(r#"exists("sub/absent.txt")"#, &[], d).unwrap());
}

#[test]
fn exists_without_a_directory_uses_the_process_cwd() {
    // `cargo test` runs integration tests from the package root.
    assert!(eval(r#"exists("Cargo.toml")"#, &[]).unwrap());
    assert!(!eval(r#"exists("no-such-file.xyz")"#, &[]).unwrap());
}

#[test]
fn exists_accepts_absolute_paths_regardless_of_directory() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let file = other.path().join("f");
    fs::write(&file, "x").unwrap();
    let expr = format!(r#"exists("{}")"#, file.display());
    assert!(eval_in(&expr, &[], Some(dir.path())).unwrap());
}

#[rstest]
#[case("true", true)]
#[case("false", false)]
#[case("!true", false)]
#[case("!false", true)]
#[case("true && true", true)]
#[case("true && false", false)]
#[case("false || true", true)]
#[case("false || false", false)]
#[case("true || false && false", true)]
#[case("(true || false) && false", false)]
#[case("!(true && false)", true)]
#[case(r#"flag("A") && !flag("B")"#, true)]
#[case(r#"flag("A") && flag("B")"#, false)]
#[case(r#"flag("B") || flag("A")"#, true)]
#[case(r#"(flag("A") || true) && flag("B")"#, false)]
#[case(r#"flag("A") && getenv("C") == "x""#, true)]
fn boolean_expressions(#[case] expr: &str, #[case] expected: bool) {
    let env = [("A", "1"), ("B", ""), ("C", "x")];
    assert_eq!(eval(expr, &env).unwrap(), expected);
}

#[test]
fn operators_inside_string_literals_are_plain_text() {
    assert!(eval(r#"getenv("X") == "a||b""#, &[("X", "a||b")]).unwrap());
    assert!(eval(r#"getenv("X") == "a && b""#, &[("X", "a && b")]).unwrap());
}

#[rstest]
#[case::empty("")]
#[case::blank("   ")]
#[case::tab_newline("\t\n")]
#[case::unknown_identifier("nonsense")]
#[case::dangling_operator(r#"getenv("X") =="#)]
#[case::unbalanced_paren("(true")]
#[case::unknown_function(r#"nope("x")"#)]
#[case::string_result(r#"getenv("X")"#)]
#[case::number_result("1 + 1")]
#[case::string_literal(r#""yes""#)]
fn malformed_or_non_boolean_expressions_error(#[case] expr: &str) {
    let err = eval(expr, &[("X", "1")]).unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err:?}");
}

#[test]
fn errors_name_the_offending_condition() {
    let msg = invalid_message("nonsense");
    assert!(
        msg.starts_with("Invalid `if` condition 'nonsense': "),
        "{msg}"
    );
    // Only the first line of the engine's message is kept.
    assert!(!msg.contains('\n'), "{msg}");
}

#[test]
fn an_empty_expression_is_reported_as_empty() {
    assert!(invalid_message("  ").contains("empty expression"));
}

#[test]
fn non_boolean_results_are_explained() {
    let msg = invalid_message("1 + 1");
    assert!(msg.contains("must evaluate to true or false"), "{msg}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn flag_matches_its_truthiness_model(value in "[ -~]{0,12}") {
        let expected = !(value.is_empty() || value == "0" || value.eq_ignore_ascii_case("false"));
        let got = eval(r#"flag("V")"#, &[("V", value.as_str())]).unwrap();
        prop_assert_eq!(got, expected);
    }

    #[test]
    fn getenv_equality_is_exact(value in "[A-Za-z0-9_ .-]{0,12}", other in "[A-Za-z0-9_ .-]{0,12}") {
        let eq = format!(r#"getenv("V") == "{other}""#);
        let ne = format!(r#"getenv("V") != "{other}""#);
        let env = [("V", value.as_str())];
        prop_assert_eq!(eval(&eq, &env).unwrap(), value == other);
        prop_assert_eq!(eval(&ne, &env).unwrap(), value != other);
    }

    #[test]
    fn negation_flips_the_result(value in "[ -~]{0,8}") {
        let env = [("V", value.as_str())];
        let plain = eval(r#"flag("V")"#, &env).unwrap();
        let negated = eval(r#"!flag("V")"#, &env).unwrap();
        prop_assert_eq!(plain, !negated);
    }

    #[test]
    fn and_or_follow_boolean_algebra(a in any::<bool>(), b in any::<bool>(), c in any::<bool>()) {
        let expr = format!("({a} && {b}) || {c}");
        prop_assert_eq!(eval(&expr, &[]).unwrap(), (a && b) || c);
        let expr = format!("!({a} || {b}) && {c}");
        prop_assert_eq!(eval(&expr, &[]).unwrap(), !(a || b) && c);
    }

    #[test]
    fn arbitrary_input_never_panics(expr in ".{0,30}") {
        let _ = eval(&expr, &[("X", "1")]);
    }

    #[test]
    fn expression_like_input_never_panics(
        expr in r#"[a-z()!&|=" ]{0,24}"#
    ) {
        let _ = eval(&expr, &[]);
    }
}
