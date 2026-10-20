//! `exec`: parameter binding, env naming and the per-task lifecycle.

#[path = "common/helpers.rs"]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{Recorder, bash};
use mq_task::config::{Config, ExecutionMode, RuntimeConfig};
use mq_task::error::Error;
use mq_task::events::TaskStatus;
use mq_task::exec::{Executor, NodeOutcome, NodeParams, TaskIo, output_var};
use mq_task::section::{CodeBlock, ParamDef, Section};
use proptest::prelude::*;
use rstest::rstest;

fn param(name: &str, default: Option<&str>) -> ParamDef {
    ParamDef {
        name: name.into(),
        default: default.map(String::from),
        ..Default::default()
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[rstest]
#[case("build", "MX_OUTPUT_BUILD")]
#[case("Build2", "MX_OUTPUT_BUILD2")]
#[case("db-migrate", "MX_OUTPUT_DB_MIGRATE")]
#[case("ns:task", "MX_OUTPUT_NS_TASK")]
#[case("a b.c", "MX_OUTPUT_A_B_C")]
#[case("", "MX_OUTPUT_")]
#[case("日本", "MX_OUTPUT___")]
fn output_var_names(#[case] title: &str, #[case] expected: &str) {
    assert_eq!(output_var(title), expected);
}

proptest! {
    #[test]
    fn output_var_is_a_valid_env_name(title in ".{0,30}") {
        let name = output_var(&title);
        let suffix = name.strip_prefix("MX_OUTPUT_").unwrap();
        prop_assert!(suffix.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'));
        // One output character per input character.
        prop_assert_eq!(suffix.chars().count(), title.chars().count());
    }
}

#[rstest]
#[case::no_params_ignores_args(vec![], vec!["a", "b"], vec![])]
#[case::positional(
    vec![param("env", None), param("tag", None)],
    vec!["prod", "v1"],
    vec![("MX_PARAM_ENV", "prod"), ("MX_PARAM_TAG", "v1")]
)]
#[case::named(
    vec![param("env", None), param("tag", None)],
    vec!["tag=v1", "env=prod"],
    vec![("MX_PARAM_ENV", "prod"), ("MX_PARAM_TAG", "v1")]
)]
#[case::named_with_dashes(
    vec![param("env", None)],
    vec!["--env=prod"],
    vec![("MX_PARAM_ENV", "prod")]
)]
#[case::mixed(
    vec![param("env", None), param("tag", None)],
    vec!["tag=v1", "prod"],
    vec![("MX_PARAM_ENV", "prod"), ("MX_PARAM_TAG", "v1")]
)]
#[case::defaults(
    vec![param("env", Some("dev")), param("tag", Some("latest"))],
    vec![],
    vec![("MX_PARAM_ENV", "dev"), ("MX_PARAM_TAG", "latest")]
)]
#[case::positional_overrides_default(
    vec![param("env", Some("dev")), param("tag", Some("latest"))],
    vec!["prod"],
    vec![("MX_PARAM_ENV", "prod"), ("MX_PARAM_TAG", "latest")]
)]
#[case::non_identifier_key_is_positional(
    vec![param("env", None)],
    vec!["https://x.test/?a=1"],
    vec![("MX_PARAM_ENV", "https://x.test/?a=1")]
)]
#[case::value_may_contain_equals(
    vec![param("q", None)],
    vec!["q=a=b"],
    vec![("MX_PARAM_Q", "a=b")]
)]
#[case::empty_named_value(
    vec![param("q", Some("d"))],
    vec!["q="],
    vec![("MX_PARAM_Q", "")]
)]
#[case::extra_args_are_ignored(
    vec![param("a", None)],
    vec!["1", "2", "3"],
    vec![("MX_PARAM_A", "1")]
)]
#[case::names_are_uppercased(
    vec![param("mixedCase", None)],
    vec!["x"],
    vec![("MX_PARAM_MIXEDCASE", "x")]
)]
fn binds_params(
    #[case] params: Vec<ParamDef>,
    #[case] args: Vec<&str>,
    #[case] expected: Vec<(&str, &str)>,
) {
    let got = Executor::bind_params(&params, &strings(&args), "task").unwrap();
    assert_eq!(got, pairs(&expected));
}

fn choice_param() -> Vec<ParamDef> {
    vec![ParamDef {
        choices: vec!["dev".into(), "prod".into()],
        ..param("env", Some("dev"))
    }]
}

#[test]
fn a_value_outside_the_choices_is_an_error_with_a_hint() {
    match Executor::bind_params(&choice_param(), &strings(&["prd"]), "deploy") {
        Err(Error::InvalidParameter(name, value, task, hint)) => {
            assert_eq!(
                (name.as_str(), value.as_str(), task.as_str()),
                ("env", "prd", "deploy")
            );
            assert_eq!(hint, "one of: dev, prod; did you mean `prod`?");
        }
        other => panic!("{other:?}"),
    }
    // Named or positional, the same rule.
    assert!(Executor::bind_params(&choice_param(), &strings(&["env=staging"]), "deploy").is_err());
}

#[test]
fn values_inside_the_choices_and_the_default_are_accepted() {
    let got = Executor::bind_params(&choice_param(), &strings(&["env=prod"]), "t").unwrap();
    assert_eq!(got, pairs(&[("MX_PARAM_ENV", "prod")]));
    let got = Executor::bind_params(&choice_param(), &[], "t").unwrap();
    assert_eq!(got, pairs(&[("MX_PARAM_ENV", "dev")]));
}

#[test]
fn an_undeclared_name_value_argument_is_an_error() {
    let params = vec![param("env", Some("staging")), param("region", None)];
    match Executor::bind_params(&params, &strings(&["regoin=eu"]), "deploy") {
        Err(Error::UnknownParameter(name, task, hint)) => {
            assert_eq!((name.as_str(), task.as_str()), ("regoin", "deploy"));
            assert!(hint.contains("did you mean `region`?"), "{hint}");
        }
        other => panic!("{other:?}"),
    }
    match Executor::bind_params(&params, &strings(&["--zone=eu"]), "deploy") {
        Err(Error::UnknownParameter(_, _, hint)) => {
            assert!(hint.contains("declared: env, region"), "{hint}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_task_without_params_still_takes_any_argument() {
    assert!(
        Executor::bind_params(&[], &strings(&["a=b"]), "t")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_missing_required_param_is_an_error() {
    let params = vec![param("env", None), param("tag", Some("x"))];
    match Executor::bind_params(&params, &[], "deploy") {
        Err(Error::MissingParameter(p, t)) => {
            assert_eq!((p.as_str(), t.as_str()), ("env", "deploy"))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_named_arg_does_not_consume_a_positional_slot() {
    // `b=2` binds b by name, so the lone positional value goes to `a`.
    let params = vec![param("a", None), param("b", None)];
    let got = Executor::bind_params(&params, &strings(&["b=2", "1"]), "t").unwrap();
    assert_eq!(got, pairs(&[("MX_PARAM_A", "1"), ("MX_PARAM_B", "2")]));
}

proptest! {
    #[test]
    fn binding_by_name_ignores_argument_order(
        values in prop::collection::vec("[a-z0-9]{0,6}", 1..6),
        rotate in 0usize..6,
    ) {
        let params: Vec<ParamDef> = (0..values.len()).map(|i| param(&format!("p{i}"), None)).collect();
        let mut args: Vec<String> = values.iter().enumerate().map(|(i, v)| format!("p{i}={v}")).collect();
        let n = args.len();
        args.rotate_left(rotate % n);
        let got = Executor::bind_params(&params, &args, "t").unwrap();
        prop_assert_eq!(got.len(), values.len());
        for (i, (k, v)) in got.iter().enumerate() {
            prop_assert_eq!(k, &format!("MX_PARAM_P{i}"));
            prop_assert_eq!(v, &values[i]);
        }
    }

    #[test]
    fn positional_binding_follows_declaration_order(
        values in prop::collection::vec("[a-z0-9]{1,6}", 1..6),
    ) {
        let params: Vec<ParamDef> = (0..values.len()).map(|i| param(&format!("p{i}"), None)).collect();
        let got = Executor::bind_params(&params, &values, "t").unwrap();
        let bound: Vec<String> = got.into_iter().map(|(_, v)| v).collect();
        prop_assert_eq!(bound, values);
    }

    #[test]
    fn one_output_per_param_and_missing_required_errors(
        n in 1usize..6,
        supplied in 0usize..6,
    ) {
        let params: Vec<ParamDef> = (0..n).map(|i| param(&format!("p{i}"), None)).collect();
        let args: Vec<String> = (0..supplied).map(|i| format!("v{i}")).collect();
        let result = Executor::bind_params(&params, &args, "t");
        if supplied >= n {
            prop_assert_eq!(result.unwrap().len(), n);
        } else {
            prop_assert!(matches!(result, Err(Error::MissingParameter(_, _))));
        }
    }
}

#[rstest]
#[case::none(None, None, None, None)]
#[case::global_only(None, None, Some("g"), Some("g"))]
#[case::task_beats_global(None, Some("t"), Some("g"), Some("t"))]
#[case::cli_beats_task(Some("c"), Some("t"), Some("g"), Some("c"))]
#[case::cli_beats_global(Some("c"), None, Some("g"), Some("c"))]
fn effective_dir_precedence(
    #[case] cli: Option<&str>,
    #[case] task: Option<&str>,
    #[case] global: Option<&str>,
    #[case] expected: Option<&str>,
) {
    let mut exec = Executor::new(Config::default());
    exec.working_dir = cli.map(PathBuf::from);
    exec.global_dir = global.map(PathBuf::from);
    let mut section = bash("t", &[], "true");
    section.dir = task.map(PathBuf::from);
    assert_eq!(exec.effective_dir(&section), expected.map(Path::new));
}

struct Node {
    outcome: Result<NodeOutcome, Error>,
    rec: Recorder,
}

impl Node {
    fn statuses(&self, name: &str) -> Vec<TaskStatus> {
        self.rec.statuses(name)
    }

    fn done(&self) -> Option<String> {
        match &self.outcome {
            Ok(NodeOutcome::Done { output }) => output.clone(),
            other => panic!("expected Done, got {other:?}"),
        }
    }
}

fn executor() -> Executor {
    let mut exec = Executor::new(Config::default());
    exec.use_cache = false;
    exec
}

fn node(exec: &Executor, section: &Section, args: &[&str], capture: bool) -> Node {
    node_with(
        exec,
        section,
        args,
        capture,
        &AtomicBool::new(false),
        &[],
        None,
    )
}

fn node_with(
    exec: &Executor,
    section: &Section,
    args: &[&str],
    capture: bool,
    cancel: &AtomicBool,
    extra_env: &[(&str, &str)],
    lang_filter: Option<&str>,
) -> Node {
    let rec = Recorder::default();
    let args = strings(args);
    let io = TaskIo {
        name: &section.title,
        observer: &rec,
        piped: true,
        capture,
        cancel,
        deadline: None,
    };
    let outcome = exec.run_node(
        &NodeParams {
            section,
            label: &section.title,
            args: &args,
            lang_filter,
            extra_env: pairs(extra_env),
            dependency: false,
            loop_source: None,
            defers: &[],
        },
        &io,
    );
    Node { outcome, rec }
}

#[test]
fn reports_started_and_succeeded() {
    let n = node(&executor(), &bash("t", &[], "echo hi"), &[], false);
    assert_eq!(n.done(), None);
    let st = n.statuses("t");
    assert!(matches!(st[0], TaskStatus::Started { dependency: false }));
    assert!(matches!(st[1], TaskStatus::Succeeded { .. }));
    assert_eq!(st.len(), 2);
    assert_eq!(n.rec.stdout("t"), vec!["hi"]);
}

#[test]
fn the_dependency_flag_is_forwarded() {
    let exec = executor();
    let section = bash("t", &[], "true");
    let rec = Recorder::default();
    let cancel = AtomicBool::new(false);
    let io = TaskIo {
        name: "t",
        observer: &rec,
        piped: true,
        capture: false,
        cancel: &cancel,
        deadline: None,
    };
    exec.run_node(
        &NodeParams {
            section: &section,
            label: &section.title,
            args: &[],
            lang_filter: None,
            extra_env: vec![],
            dependency: true,
            loop_source: None,
            defers: &[],
        },
        &io,
    )
    .unwrap();
    assert!(matches!(
        rec.statuses("t")[0],
        TaskStatus::Started { dependency: true }
    ));
}

#[rstest]
#[case("hello\n", "hello")]
#[case("hello\n\n\n", "hello")]
#[case("a\nb\n", "a\nb")]
#[case("", "")]
fn captured_output_is_trimmed(#[case] printed: &str, #[case] expected: &str) {
    let code = format!("printf '{}'", printed.replace('\n', "\\n"));
    let n = node(&executor(), &bash("t", &[], &code), &[], true);
    assert_eq!(n.done().as_deref(), Some(expected));
}

#[test]
fn output_is_not_captured_unless_requested() {
    let n = node(&executor(), &bash("t", &[], "echo x"), &[], false);
    assert_eq!(n.done(), None);
}

#[test]
fn captured_output_is_still_forwarded_to_the_observer() {
    let n = node(
        &executor(),
        &bash("t", &[], "echo one; echo two"),
        &[],
        true,
    );
    assert_eq!(n.done().as_deref(), Some("one\ntwo"));
    assert_eq!(n.rec.stdout("t"), vec!["one", "two"]);
}

#[rstest]
#[case("true", true)]
#[case("false", false)]
#[case(r#"getenv("FLAG") == "on""#, true)]
#[case(r#"getenv("FLAG") == "off""#, false)]
#[case(r#"getenv("MX_PARAM_ENV") == "prod""#, true)]
#[case(r#"flag("OVERRIDE")"#, true)]
#[case(r#"flag("FROM_DEP")"#, true)]
fn conditions_decide_whether_a_task_runs(#[case] cond: &str, #[case] runs: bool) {
    let mut exec = executor();
    exec.env_overrides = pairs(&[("OVERRIDE", "1")]);
    let mut s = bash("t", &[], "echo ran");
    s.condition = Some(cond.into());
    s.env = pairs(&[("FLAG", "on")]);
    s.params = vec![param("env", None)];
    let n = node_with(
        &exec,
        &s,
        &["prod"],
        false,
        &AtomicBool::new(false),
        &[("FROM_DEP", "1")],
        None,
    );
    assert!(n.outcome.is_ok(), "{:?}", n.outcome);
    if runs {
        assert_eq!(n.rec.stdout("t"), vec!["ran"]);
    } else {
        assert_eq!(n.outcome.as_ref().unwrap(), &NodeOutcome::Skipped);
        assert!(n.rec.stdout("t").is_empty());
        assert!(n.statuses("t").iter().any(|s| matches!(
            s,
            TaskStatus::Skipped { reason } if reason == &format!("if: {cond}")
        )));
    }
}

#[test]
fn an_invalid_condition_fails_the_task() {
    let mut s = bash("t", &[], "echo ran");
    s.condition = Some("???".into());
    let n = node(&executor(), &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::Config(_))));
    assert!(n.rec.stdout("t").is_empty());
    assert!(
        n.statuses("t")
            .iter()
            .any(|s| matches!(s, TaskStatus::Failed { .. }))
    );
}

#[test]
fn a_missing_required_param_fails_before_running() {
    let mut s = bash("t", &[], "echo ran");
    s.params = vec![param("env", None)];
    let n = node(&executor(), &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::MissingParameter(_, _))));
    assert!(n.rec.stdout("t").is_empty());
}

#[test]
fn prompts_cannot_be_asked_when_output_is_piped() {
    // Parallel runs and the TUI pipe task output and close stdin, so a
    // `prompt` can only be satisfied with `--yes`.
    let mut s = bash("t", &[], "echo ran");
    s.prompt = Some("Sure?".into());

    let n = node(&executor(), &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::ConfirmationRequired(ref t)) if t == "t"));
    assert!(n.rec.stdout("t").is_empty());
    assert!(
        n.statuses("t")
            .iter()
            .any(|s| matches!(s, TaskStatus::Failed { error } if error.contains("--yes")))
    );

    let mut exec = executor();
    exec.assume_yes = true;
    let n = node(&exec, &s, &[], false);
    assert_eq!(n.done(), None);
    assert_eq!(n.rec.stdout("t"), vec!["ran"]);
}

#[rstest]
#[case::exit_code("exit 5", 5)]
#[case::exit_one("false", 1)]
#[cfg_attr(unix, case::signal("kill -9 $$", 137))]
fn failing_commands_report_their_exit_status(#[case] code: &str, #[case] expected: i32) {
    let n = node(&executor(), &bash("t", &[], code), &[], false);
    assert!(
        matches!(n.outcome, Err(Error::ExecutionFailed(c)) if c == expected),
        "{:?}",
        n.outcome
    );
    assert!(
        n.statuses("t")
            .iter()
            .any(|s| matches!(s, TaskStatus::Failed { .. }))
    );
}

#[test]
fn blocks_run_in_order_and_stop_at_the_first_failure() {
    let mut s = bash("t", &[], "echo one");
    s.codes.push(CodeBlock {
        lang: "bash".into(),
        code: "exit 9".into(),
    });
    s.codes.push(CodeBlock {
        lang: "bash".into(),
        code: "echo three".into(),
    });
    let n = node(&executor(), &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::ExecutionFailed(9))));
    assert_eq!(n.rec.stdout("t"), vec!["one"]);
}

#[test]
fn language_filter_selects_blocks() {
    let mut s = bash("t", &[], "echo from-bash");
    s.codes.push(CodeBlock {
        lang: "sh".into(),
        code: "echo from-sh".into(),
    });
    let n = node_with(
        &executor(),
        &s,
        &[],
        false,
        &AtomicBool::new(false),
        &[],
        Some("sh"),
    );
    n.outcome.as_ref().unwrap();
    assert_eq!(n.rec.stdout("t"), vec!["from-sh"]);
}

#[test]
fn blocks_without_a_language_are_skipped() {
    let mut s = bash("t", &[], "echo run");
    s.codes.insert(
        0,
        CodeBlock {
            lang: String::new(),
            code: "echo never".into(),
        },
    );
    let n = node(&executor(), &s, &[], false);
    n.outcome.as_ref().unwrap();
    assert_eq!(n.rec.stdout("t"), vec!["run"]);
}

#[test]
fn an_unknown_runtime_is_an_error_and_is_not_retried() {
    let mut s = bash("t", &[], "x");
    s.codes[0].lang = "cobol".into();
    s.retries = 3;
    s.retry_delay = Some(Duration::from_millis(10));
    let n = node(&executor(), &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::RuntimeNotFound(ref l)) if l == "cobol"));
    assert!(
        !n.statuses("t")
            .iter()
            .any(|s| matches!(s, TaskStatus::Retrying { .. }))
    );
}

#[test]
fn retries_until_success_and_reports_each_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("n");
    // Fails twice, succeeds on the third attempt.
    let code = format!(
        "f='{}'; n=$(cat \"$f\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$f\"; [ $n -ge 3 ]",
        counter.display()
    );
    let mut s = bash("t", &[], &code);
    s.retries = 3;
    s.retry_delay = Some(Duration::from_millis(5));
    let n = node(&executor(), &s, &[], false);
    n.outcome.as_ref().unwrap();

    let retries: Vec<(u32, u32)> = n
        .statuses("t")
        .into_iter()
        .filter_map(|s| match s {
            TaskStatus::Retrying { attempt, max, .. } => Some((attempt, max)),
            _ => None,
        })
        .collect();
    assert_eq!(retries, vec![(1, 4), (2, 4)]);
    assert_eq!(fs::read_to_string(&counter).unwrap().trim(), "3");
}

#[test]
fn retries_stop_after_the_configured_number() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let code = format!("echo x >> '{}'; exit 1", log.display());
    let mut s = bash("t", &[], &code);
    s.retries = 2;
    s.retry_delay = Some(Duration::from_millis(5));
    let n = node(&executor(), &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::ExecutionFailed(1))));
    // 1 initial attempt + 2 retries.
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 3);
}

#[test]
fn the_retry_delay_is_reported_and_honoured() {
    let mut s = bash("t", &[], "exit 1");
    s.retries = 1;
    s.retry_delay = Some(Duration::from_millis(150));
    let t = Instant::now();
    let n = node(&executor(), &s, &[], false);
    assert!(n.outcome.is_err());
    assert!(t.elapsed() >= Duration::from_millis(150));
    assert!(n.statuses("t").iter().any(|s| matches!(
        s,
        TaskStatus::Retrying { delay, .. } if *delay == Duration::from_millis(150)
    )));
}

#[test]
fn timeouts_kill_the_task() {
    let mut s = bash("slow", &[], "sleep 30");
    s.timeout = Some(Duration::from_millis(200));
    let t = Instant::now();
    let n = node(&executor(), &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::Timeout(ref name)) if name == "slow"));
    assert!(t.elapsed() < Duration::from_secs(10));
}

#[test]
fn timeouts_are_retried() {
    let mut s = bash("slow", &[], "sleep 30");
    s.timeout = Some(Duration::from_millis(150));
    s.retries = 1;
    s.retry_delay = Some(Duration::from_millis(5));
    let n = node(&executor(), &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::Timeout(_))));
    assert!(
        n.statuses("slow")
            .iter()
            .any(|s| matches!(s, TaskStatus::Retrying { .. }))
    );
}

#[test]
fn cancellation_kills_the_task_and_reports_cancelled() {
    let exec = executor();
    let s = bash("t", &[], "sleep 30");
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    let n = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            cancel.store(true, Ordering::SeqCst);
        });
        node_with(&exec, &s, &[], false, &cancel, &[], None)
    });
    assert!(matches!(n.outcome, Err(Error::Cancelled)));
    assert!(t.elapsed() < Duration::from_secs(10));
    assert!(
        n.statuses("t")
            .iter()
            .any(|s| matches!(s, TaskStatus::Cancelled { .. }))
    );
}

#[test]
fn dry_run_prints_instead_of_executing() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let mut exec = executor();
    exec.dry_run = true;
    let s = bash("t", &[], &format!("touch '{}'", marker.display()));
    let n = node(&exec, &s, &[], true);
    assert_eq!(n.done().as_deref(), Some(""));
    assert!(!marker.exists());
}

fn env_value(exec: &Executor, section: &Section, args: &[&str], var: &str) -> String {
    let mut s = section.clone();
    s.codes = vec![CodeBlock {
        lang: "bash".into(),
        code: format!("printf '%s' \"${{{var}-<unset>}}\""),
    }];
    node(exec, &s, args, true).done().unwrap()
}

#[test]
fn env_layers_apply_in_increasing_precedence() {
    let mut exec = executor();
    exec.global_env = pairs(&[("K", "global"), ("ONLY_GLOBAL", "g")]);
    let mut s = bash("t", &[], "");
    s.env = pairs(&[("K", "task")]);

    assert_eq!(env_value(&exec, &s, &[], "K"), "task");
    assert_eq!(env_value(&exec, &s, &[], "ONLY_GLOBAL"), "g");

    exec.env_overrides = pairs(&[("K", "cli")]);
    assert_eq!(env_value(&exec, &s, &[], "K"), "cli");
}

#[test]
fn args_are_exposed_as_mx_args_and_mx_arg_n() {
    let exec = executor();
    let s = bash("t", &[], "");
    assert_eq!(env_value(&exec, &s, &["a", "b c"], "MX_ARGS"), "a b c");
    assert_eq!(env_value(&exec, &s, &["a", "b c"], "MX_ARG_0"), "a");
    assert_eq!(env_value(&exec, &s, &["a", "b c"], "MX_ARG_1"), "b c");
    assert_eq!(env_value(&exec, &s, &[], "MX_ARGS"), "<unset>");
}

#[test]
fn params_are_exposed_as_mx_param() {
    let exec = executor();
    let mut s = bash("t", &[], "");
    s.params = vec![param("tag", None), param("env", Some("dev"))];
    assert_eq!(env_value(&exec, &s, &["v1"], "MX_PARAM_TAG"), "v1");
    assert_eq!(env_value(&exec, &s, &["v1"], "MX_PARAM_ENV"), "dev");
}

#[test]
fn extra_env_from_dependencies_is_visible() {
    let exec = executor();
    let s = bash("t", &[], "printf '%s' \"$MX_OUTPUT_DEP\"");
    let n = node_with(
        &exec,
        &s,
        &[],
        true,
        &AtomicBool::new(false),
        &[("MX_OUTPUT_DEP", "from-dep")],
        None,
    );
    assert_eq!(n.done().as_deref(), Some("from-dep"));
}

#[test]
fn the_working_directory_is_applied() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = fs::canonicalize(dir.path()).unwrap();
    let mut exec = executor();
    exec.working_dir = Some(dir.path().to_path_buf());
    let s = bash("t", &[], "pwd -P");
    let n = node(&exec, &s, &[], true);
    assert_eq!(n.done().unwrap(), canonical.display().to_string());
}

#[rstest]
#[case::stdin(ExecutionMode::Stdin, "sh")]
#[case::file(ExecutionMode::File, "sh")]
#[case::arg(ExecutionMode::Arg, "sh -c")]
fn every_execution_mode_runs_the_code(#[case] mode: ExecutionMode, #[case] command: &str) {
    let mut config = Config::default();
    config.runtimes.insert(
        "shx".into(),
        RuntimeConfig::Detailed {
            command: command.into(),
            execution_mode: mode,
        },
    );
    let mut exec = Executor::new(config);
    exec.use_cache = false;
    let mut s = bash("t", &[], "");
    s.codes[0] = CodeBlock {
        lang: "shx".into(),
        code: "echo mode-ok".into(),
    };
    let n = node(&exec, &s, &[], true);
    assert_eq!(n.done().as_deref(), Some("mode-ok"));
}

#[test]
fn a_missing_binary_is_an_execution_error() {
    let mut config = Config::default();
    config.runtimes.insert(
        "ghost".into(),
        RuntimeConfig::Simple("mq-task-no-such-binary-xyz".into()),
    );
    let exec = Executor::new(config);
    let mut s = bash("t", &[], "");
    s.codes[0] = CodeBlock {
        lang: "ghost".into(),
        code: "x".into(),
    };
    let n = node(&exec, &s, &[], false);
    assert!(
        matches!(n.outcome, Err(Error::Execution(_))),
        "{:?}",
        n.outcome
    );
}

#[test]
fn an_empty_runtime_command_is_rejected() {
    let mut config = Config::default();
    config
        .runtimes
        .insert("blank".into(), RuntimeConfig::Simple("  ".into()));
    let exec = Executor::new(config);
    let mut s = bash("t", &[], "");
    s.codes[0] = CodeBlock {
        lang: "blank".into(),
        code: "x".into(),
    };
    let n = node(&exec, &s, &[], false);
    assert!(matches!(n.outcome, Err(Error::RuntimeNotFound(_))));
}

#[test]
fn required_variables_must_be_set_and_not_empty() {
    let mut s = bash("t", &[], "echo ran");
    s.requires = vec!["MQ_TASK_TEST_REQ_A".into(), "MQ_TASK_TEST_REQ_B".into()];

    let n = node(&executor(), &s, &[], false);
    match &n.outcome {
        Err(Error::MissingVariable(vars, task)) => {
            assert_eq!(vars, "MQ_TASK_TEST_REQ_A, MQ_TASK_TEST_REQ_B");
            assert_eq!(task, "t");
        }
        other => panic!("expected MissingVariable, got {other:?}"),
    }
    assert!(n.rec.stdout("t").is_empty());

    // An empty value does not count; a value from `--env`/meta/extra env does.
    let n = node_with(
        &executor(),
        &s,
        &[],
        false,
        &AtomicBool::new(false),
        &[("MQ_TASK_TEST_REQ_A", ""), ("MQ_TASK_TEST_REQ_B", "x")],
        None,
    );
    assert!(
        matches!(n.outcome, Err(Error::MissingVariable(ref v, _)) if v == "MQ_TASK_TEST_REQ_A")
    );

    let n = node_with(
        &executor(),
        &s,
        &[],
        false,
        &AtomicBool::new(false),
        &[("MQ_TASK_TEST_REQ_A", "1"), ("MQ_TASK_TEST_REQ_B", "2")],
        None,
    );
    assert!(n.outcome.is_ok());
    assert_eq!(n.rec.stdout("t"), vec!["ran"]);
}

fn source_node(section: &Section, output: Option<&str>, capture: bool) -> Node {
    let exec = executor();
    let rec = Recorder::default();
    let cancel = AtomicBool::new(false);
    let io = TaskIo {
        name: &section.title,
        observer: &rec,
        piped: true,
        capture,
        cancel: &cancel,
        deadline: None,
    };
    let extra_env = output
        .map(|o| vec![(output_var("list"), o.to_string())])
        .unwrap_or_default();
    let outcome = exec.run_node(
        &NodeParams {
            section,
            label: &section.title,
            args: &[],
            lang_filter: None,
            extra_env,
            dependency: false,
            loop_source: Some("list"),
            defers: &[],
        },
        &io,
    );
    Node { outcome, rec }
}

#[test]
fn a_from_loop_runs_once_per_line_of_the_source_output() {
    let mut s = bash(
        "each",
        &[],
        "echo \"item=$MX_ITEM index=$MX_INDEX name=$MX_NAME\"",
    );
    s.loop_var = Some("name".into());
    let n = source_node(&s, Some("a\r\n\n  \nb c\n"), false);
    assert_eq!(n.done(), None);
    assert_eq!(
        n.rec.stdout("each"),
        vec![
            "↳ each[a]",
            "item=a index=0 name=a",
            "↳ each[b c]",
            "item=b c index=1 name=b c",
        ]
    );
}

#[test]
fn a_from_loop_stops_at_the_first_failing_item() {
    let s = bash(
        "each",
        &[],
        "echo \"ran $MX_ITEM\"; [ \"$MX_ITEM\" != b ] || exit 4",
    );
    let n = source_node(&s, Some("a\nb\nc"), false);
    assert!(matches!(n.outcome, Err(Error::ExecutionFailed(4))));
    let out = n.rec.stdout("each").join("\n");
    assert!(out.contains("ran a") && out.contains("ran b") && !out.contains("ran c"));
}

#[test]
fn a_from_loop_without_items_is_skipped() {
    let s = bash("each", &[], "echo ran");
    for output in [None, Some(""), Some("\n \n")] {
        let n = source_node(&s, output, false);
        assert!(matches!(n.outcome, Ok(NodeOutcome::Skipped)));
        assert!(n.rec.stdout("each").is_empty());
        assert!(n.statuses("each").iter().any(|st| matches!(
            st,
            TaskStatus::Skipped { reason } if reason == "for: no items"
        )));
    }
}

#[test]
fn a_from_loop_captures_the_output_of_every_item() {
    let s = bash("each", &[], "echo \"got $MX_ITEM\"");
    let n = source_node(&s, Some("x\ny"), true);
    assert_eq!(n.done().as_deref(), Some("got x\ngot y"));
}

#[test]
fn each_item_of_a_from_loop_is_retried() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("seen");
    let mut s = bash(
        "each",
        &[],
        &format!(
            "if [ -e {m} ]; then echo recovered; else touch {m}; exit 1; fi",
            m = marker.display()
        ),
    );
    s.retries = 1;
    s.retry_delay = Some(Duration::from_millis(10));
    let n = source_node(&s, Some("only"), false);
    assert_eq!(n.done(), None);
    assert!(n.rec.stdout("each").contains(&"recovered".to_string()));
}

fn stderr_lines(rec: &Recorder, task: &str) -> Vec<String> {
    rec.events()
        .into_iter()
        .filter_map(|e| match e {
            common::Event::Output(t, mq_task::events::Stream::Stderr, l) if t == task => Some(l),
            _ => None,
        })
        .collect()
}

#[test]
fn verbose_shows_each_block_before_running_it() {
    let mut s = bash("t", &[], "echo one\n\necho two\n");
    s.codes.push(CodeBlock {
        lang: "sh".into(),
        code: "echo three".into(),
    });
    let mut exec = executor();
    exec.verbose = true;
    let n = node(&exec, &s, &[], false);
    assert_eq!(n.done(), None);
    assert_eq!(
        stderr_lines(&n.rec, "t"),
        vec![
            "▸ bash",
            "  echo one",
            "",
            "  echo two",
            "▸ sh",
            "  echo three"
        ]
    );
    // The program's own output is untouched.
    assert_eq!(n.rec.stdout("t"), vec!["one", "two", "three"]);
}

#[test]
fn verbose_is_quiet_by_default_and_in_dry_runs() {
    let s = bash("t", &[], "echo hi");
    let n = node(&executor(), &s, &[], false);
    assert!(stderr_lines(&n.rec, "t").is_empty());

    let mut exec = executor();
    exec.verbose = true;
    exec.dry_run = true;
    let n = node(&exec, &s, &[], false);
    assert!(
        stderr_lines(&n.rec, "t").is_empty(),
        "dry-run prints its own"
    );
}

#[test]
fn verbose_names_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = bash("t", &[], "true");
    s.dir = Some(dir.path().to_path_buf());
    let mut exec = executor();
    exec.verbose = true;
    let n = node(&exec, &s, &[], false);
    assert_eq!(
        stderr_lines(&n.rec, "t")[0],
        format!("▸ bash in {}", dir.path().display())
    );
}
