//! `Runner`: markdown parsing, `meta` handling, imports, graphs and runs.

#[path = "common/helpers.rs"]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use common::Recorder;
use mq_task::events::TaskStatus;
use mq_task::{Config, Error, GraphFormat, RunOptions, Runner};
use proptest::prelude::*;
use rstest::rstest;

fn runner() -> Runner {
    let mut r = Runner::new(Config::default());
    r.set_use_cache(false);
    r
}

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    path
}

#[rstest]
#[case(&["A=1"], &[("A", "1")])]
#[case(&["A=1", "B=2"], &[("A", "1"), ("B", "2")])]
#[case(&["A="], &[("A", "")])]
#[case(&["A=b=c"], &[("A", "b=c")])]
#[case(&["A= spaced "], &[("A", " spaced ")])]
#[case(&[], &[])]
fn parses_env_overrides(#[case] raw: &[&str], #[case] expected: &[(&str, &str)]) {
    let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
    let got = Runner::parse_env_overrides(&raw).unwrap();
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(got, expected);
}

#[rstest]
#[case("NOEQUALS")]
#[case("=value")]
#[case("")]
fn rejects_malformed_env_overrides(#[case] raw: &str) {
    let err = Runner::parse_env_overrides(&[raw.to_string()]).unwrap_err();
    assert!(matches!(&err, Error::InvalidEnv(v) if v == raw), "{err:?}");
}

#[test]
fn one_bad_env_override_fails_the_batch() {
    let raw = vec!["A=1".to_string(), "broken".to_string()];
    assert!(Runner::parse_env_overrides(&raw).is_err());
}

proptest! {
    #[test]
    fn env_overrides_round_trip(
        key in "[A-Za-z_][A-Za-z0-9_]{0,10}",
        value in "[ -~]{0,20}",
    ) {
        let got = Runner::parse_env_overrides(&[format!("{key}={value}")]).unwrap();
        prop_assert_eq!(got, vec![(key, value)]);
    }

    #[test]
    fn an_override_without_equals_is_always_invalid(raw in "[^=]{0,20}") {
        prop_assert!(Runner::parse_env_overrides(&[raw]).is_err());
    }
}

#[test]
fn extracts_titles_descriptions_and_code_blocks_in_order() {
    let md = "\
# Project

## build

Compiles everything.

```bash
echo one
```

```python
print(2)
```

## empty

Only prose here.
";
    let sections = runner().extract_sections(md).unwrap();
    let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["Project", "build", "empty"]);

    let build = &sections[1];
    assert_eq!(build.description.as_deref(), Some("Compiles everything."));
    let langs: Vec<(&str, &str)> = build
        .codes
        .iter()
        .map(|c| (c.lang.as_str(), c.code.as_str()))
        .collect();
    assert_eq!(langs, [("bash", "echo one"), ("python", "print(2)")]);
    assert!(sections[2].codes.is_empty());
}

#[test]
fn meta_blocks_are_not_code() {
    let md = "## t\n\n```meta\ndepends = [\"x\"]\n```\n\n```bash\ntrue\n```\n";
    let s = &runner().extract_sections(md).unwrap()[0];
    assert_eq!(s.codes.len(), 1);
    assert_eq!(s.codes[0].lang, "bash");
}

#[test]
fn list_task_sections_hides_headings_that_do_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "t.md",
        "\
# Title

## runs

```bash
true
```

## aggregate

```meta
depends = [\"runs\"]
```

## prose

Just words.
",
    );
    let mut r = runner();
    let names: Vec<String> = r
        .list_task_sections(&file)
        .unwrap()
        .into_iter()
        .map(|s| s.title)
        .collect();
    assert_eq!(names, ["runs", "aggregate"]);

    // The unfiltered view keeps everything.
    assert_eq!(r.load_sections(&file).unwrap().len(), 4);
    let listed = r.list_tasks(&file).unwrap();
    assert_eq!(listed.len(), 4);
    assert!(listed.iter().any(|l| l.starts_with("prose: ")));
}

#[test]
fn parses_every_meta_key() {
    let md = r#"
## deploy

```meta
depends = ["build", "test"]
params = ["env=staging", "verbose"]
alias = ["d", "ship"]
private = true
env = ["REGION=eu", "DEBUG=a=b"]
dir = "services/api"
parallel = true
if = 'getenv("CI") == "1"'
retries = 3
retry_delay = "250ms"
timeout = "1.5m"
capture = true
sources = ["src/**/*.rs", "Cargo.toml"]
outputs = ["target/app"]
```

```bash
true
```
"#;
    let s = runner().extract_sections(md).unwrap().remove(0);
    assert_eq!(s.depends, ["build", "test"]);
    assert_eq!(s.params.len(), 2);
    assert_eq!(
        (s.params[0].name.as_str(), s.params[0].default.as_deref()),
        ("env", Some("staging"))
    );
    assert_eq!(
        (s.params[1].name.as_str(), s.params[1].default.as_deref()),
        ("verbose", None)
    );
    assert_eq!(s.aliases, ["d", "ship"]);
    assert!(s.private);
    assert_eq!(
        s.env,
        [
            ("REGION".to_string(), "eu".to_string()),
            ("DEBUG".to_string(), "a=b".to_string())
        ]
    );
    assert_eq!(s.dir, Some(PathBuf::from("services/api")));
    assert!(s.parallel);
    assert_eq!(s.condition.as_deref(), Some(r#"getenv("CI") == "1""#));
    assert_eq!(s.retries, 3);
    assert_eq!(s.retry_delay, Some(Duration::from_millis(250)));
    assert_eq!(s.timeout, Some(Duration::from_secs(90)));
    assert!(s.capture);
    assert_eq!(s.sources, ["src/**/*.rs", "Cargo.toml"]);
    assert_eq!(s.outputs, ["target/app"]);
}

#[test]
fn a_task_without_meta_gets_defaults() {
    let s = runner()
        .extract_sections("## t\n\n```bash\ntrue\n```\n")
        .unwrap()
        .remove(0);
    assert!(s.depends.is_empty() && s.params.is_empty() && s.aliases.is_empty());
    assert!(!s.private && !s.parallel && !s.capture);
    assert_eq!(s.retries, 0);
    assert_eq!(
        (s.retry_delay, s.timeout, s.condition, s.dir),
        (None, None, None, None)
    );
}

#[rstest]
#[case("_helper", true)]
#[case("helper_", false)]
#[case("helper", false)]
fn underscore_titles_are_private(#[case] title: &str, #[case] private: bool) {
    let md = format!("## {title}\n\n```bash\ntrue\n```\n");
    let s = runner().extract_sections(&md).unwrap().remove(0);
    assert_eq!(s.private, private);
}

#[rstest]
#[case("env = [\"x=1\", \"y=2\"]", 2)]
#[case("env = []", 0)]
fn env_lists_are_parsed(#[case] meta: &str, #[case] count: usize) {
    let md = format!("## t\n\n```meta\n{meta}\n```\n\n```bash\ntrue\n```\n");
    assert_eq!(runner().extract_sections(&md).unwrap()[0].env.len(), count);
}

#[rstest]
#[case::bad_timeout("timeout = \"soon\"", "Invalid duration")]
#[case::bad_retry_delay("retry_delay = \"-1s\"", "Invalid duration")]
#[case::overflowing_timeout("timeout = \"99999999999999999999999999h\"", "Invalid duration")]
#[case::wrong_type("retries = \"many\"", "task 't'")]
#[case::not_toml("depends = = [", "task 't'")]
#[case::bad_env("env = [\"NOEQUALS\"]", "NOEQUALS")]
fn invalid_meta_is_reported_with_the_task_name(#[case] meta: &str, #[case] needle: &str) {
    let md = format!("## t\n\n```meta\n{meta}\n```\n\n```bash\ntrue\n```\n");
    let err = runner().extract_sections(&md).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(needle), "{msg}");
}

#[test]
fn unknown_meta_keys_are_rejected_so_typos_are_not_silently_ignored() {
    let md = "## t\n\n```meta\nfuture_option = true\n```\n\n```bash\ntrue\n```\n";
    let err = runner().extract_sections(md).unwrap_err();
    assert!(
        err.to_string().contains("unknown key `future_option`"),
        "{err}"
    );
}

#[test]
fn document_level_env_and_dir_apply_to_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "t.md",
        "\
```meta
env = [\"GLOBAL=1\"]
dir = \"work\"
```

## a

```bash
echo \"$GLOBAL\"
```
",
    );
    let sections = runner().load_sections(&file).unwrap();
    assert_eq!(sections.len(), 1);

    // The defaults take effect when the task runs.
    let rec = Arc::new(Recorder::default());
    fs::create_dir(dir.path().join("work")).unwrap();
    let mut r = runner();
    // `dir` is relative to the process CWD, so point it somewhere real.
    r.set_working_dir(Some(dir.path().to_path_buf()));
    r.run_task_with_options(
        &file,
        "a",
        RunOptions {
            observer: Some(rec.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rec.stdout("a"), ["1"]);
}

#[test]
fn an_invalid_document_meta_block_is_reported() {
    let md = "```meta\nenv = [\"NOEQUALS\"]\n```\n\n## a\n\n```bash\ntrue\n```\n";
    assert!(runner().extract_sections(md).is_err());
}

const GRAPH_DOC: &str = "\
## lint

```bash
true
```

## build

```meta
retries = 2
```

```bash
true
```

## test

```meta
depends = [\"build\"]
if = 'os() == \"linux\"'
```

```bash
true
```

## _internal

```bash
true
```

## ci

```meta
depends = [\"lint\", \"test\", \"_internal\"]
parallel = true
```
";

#[test]
fn dependency_graph_text_for_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", GRAPH_DOC);
    let out = runner()
        .dependency_graph(&file, Some("ci"), GraphFormat::Text)
        .unwrap();
    assert_eq!(
        out,
        "ci [parallel]\n├── lint\n├── test [if: os() == \"linux\"]\n│   └── build [retries: 2]\n└── _internal\n"
    );
}

#[test]
fn dependency_graph_mermaid_for_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", GRAPH_DOC);
    let out = runner()
        .dependency_graph(&file, Some("ci"), GraphFormat::Mermaid)
        .unwrap();
    assert!(out.starts_with("graph TD\n"));
    assert!(out.contains("[\"ci [parallel]\"]"), "{out}");
    assert!(out.contains("[\"build [retries: 2]\"]"), "{out}");
    assert!(
        out.contains("[\"test [if: os() == #quot;linux#quot;]\"]"),
        "{out}"
    );
    assert_eq!(out.matches("-->").count(), 4, "{out}");
}

#[test]
fn dependency_graph_without_a_task_starts_from_public_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", GRAPH_DOC);
    let out = runner()
        .dependency_graph(&file, None, GraphFormat::Text)
        .unwrap();
    let roots: Vec<&str> = out
        .lines()
        .filter(|l| !l.starts_with(['├', '└', '│', ' ']))
        .collect();
    // `_internal` is private so it is not a root of its own.
    assert_eq!(
        roots,
        [
            "lint",
            "build [retries: 2]",
            "test [if: os() == \"linux\"]",
            "ci [parallel]"
        ]
    );
}

#[test]
fn dependency_graph_validates_the_requested_task() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "t.md",
        "## a\n\n```meta\ndepends = [\"b\"]\n```\n\n## b\n\n```meta\ndepends = [\"a\"]\n```\n",
    );
    let mut r = runner();
    assert!(matches!(
        r.dependency_graph(&file, Some("a"), GraphFormat::Mermaid),
        Err(Error::CircularDependency(_))
    ));
    assert!(matches!(
        r.dependency_graph(&file, Some("zzz"), GraphFormat::Mermaid),
        Err(Error::SectionNotFound(_))
    ));
}

#[test]
fn dependency_graph_of_a_missing_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.md");
    assert!(matches!(
        runner().dependency_graph(&missing, None, GraphFormat::Text),
        Err(Error::Io(_))
    ));
}

fn import_fixture(dir: &Path) -> PathBuf {
    write(
        dir,
        "lib.md",
        "\
```meta
env = [\"FROM_LIB=lib\"]
```

# Library

## helper

```bash
echo \"helper:$FROM_LIB\"
```

## packaged

```meta
depends = [\"helper\", \"tools:external\"]
alias = [\"pkg\"]
```

```bash
echo packaged
```
",
    );
    write(
        dir,
        "main.md",
        "\
```meta
imports = [\"lib.md\"]
```

## all

```meta
depends = [\"lib:packaged\"]
```

```bash
echo all
```
",
    )
}

#[test]
fn imported_tasks_are_namespaced_by_file_stem() {
    let dir = tempfile::tempdir().unwrap();
    let main = import_fixture(dir.path());
    let sections = runner().load_sections(&main).unwrap();
    let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
    // The imported file's document title (a do-nothing heading) is dropped.
    assert_eq!(titles, ["all", "lib:helper", "lib:packaged"]);

    let packaged = &sections[2];
    // Intra-file references and aliases are qualified; external ones are not.
    assert_eq!(packaged.depends, ["lib:helper", "tools:external"]);
    assert_eq!(packaged.aliases, ["lib:pkg"]);
}

#[test]
fn imported_tasks_keep_their_own_document_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let main = import_fixture(dir.path());
    let sections = runner().load_sections(&main).unwrap();
    let helper = sections.iter().find(|s| s.title == "lib:helper").unwrap();
    assert_eq!(helper.env, [("FROM_LIB".to_string(), "lib".to_string())]);
    // ...but they do not leak into the importing file's own tasks.
    assert!(sections[0].env.is_empty());
}

#[test]
fn missing_imports_are_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let main = write(
        dir.path(),
        "main.md",
        "```meta\nimports = [\"nope.md\"]\n```\n\n## a\n\n```bash\ntrue\n```\n",
    );
    assert!(matches!(runner().load_sections(&main), Err(Error::Io(_))));
}

#[test]
fn two_imports_with_the_same_stem_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a/tasks.md", "## x\n\n```bash\ntrue\n```\n");
    write(dir.path(), "b/tasks.md", "## y\n\n```bash\ntrue\n```\n");
    let main = write(
        dir.path(),
        "main.md",
        "```meta\nimports = [\"a/tasks.md\", \"b/tasks.md\"]\n```\n\n## m\n\n```bash\ntrue\n```\n",
    );
    let err = runner().load_sections(&main).unwrap_err();
    assert!(err.to_string().contains("share the namespace"), "{err}");
}

#[test]
fn imported_graphs_use_qualified_names() {
    let dir = tempfile::tempdir().unwrap();
    let main = import_fixture(dir.path());
    let out = runner()
        .dependency_graph(&main, Some("all"), GraphFormat::Mermaid)
        .unwrap_err();
    // `tools:external` is not defined anywhere, so planning fails loudly.
    assert!(matches!(out, Error::SectionNotFound(ref n) if n == "tools:external"));
}

#[test]
fn imported_tasks_run_with_their_defaults() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "lib.md",
        "```meta\nenv = [\"FROM_LIB=lib\"]\n```\n\n## helper\n\n```bash\necho \"helper:$FROM_LIB\"\n```\n",
    );
    let main = write(
        dir.path(),
        "main.md",
        "```meta\nimports = [\"lib.md\"]\n```\n\n## all\n\n```meta\ndepends = [\"lib:helper\"]\n```\n\n```bash\necho \"all:${FROM_LIB:-unset}\"\n```\n",
    );
    let rec = Arc::new(Recorder::default());
    runner()
        .run_task_with_options(
            &main,
            "all",
            RunOptions {
                observer: Some(rec.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(rec.stdout("lib:helper"), ["helper:lib"]);
    assert_eq!(rec.stdout("all"), ["all:unset"]);
}

const RUN_DOC: &str = "\
## a

```bash
echo A
```

## b

```meta
depends = [\"a\"]
```

```bash
echo B $MX_ARGS
```
";

#[test]
fn run_reports_the_plan_and_every_task() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", RUN_DOC);
    let rec = Arc::new(Recorder::default());
    runner()
        .run_task_with_options(
            &file,
            "b",
            RunOptions {
                args: vec!["x".into(), "y".into()],
                observer: Some(rec.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(matches!(
        rec.events().first(),
        Some(common::Event::Plan(names)) if names == &["a", "b"]
    ));
    assert_eq!(rec.stdout("a"), ["A"]);
    assert_eq!(rec.stdout("b"), ["B x y"]);
    for t in ["a", "b"] {
        assert!(
            rec.statuses(t)
                .iter()
                .any(|s| matches!(s, TaskStatus::Succeeded { .. }))
        );
    }
}

#[rstest]
#[case::default(None, false)]
#[case::one(Some(1), false)]
#[case::several(Some(4), false)]
#[case::forced_parallel(None, true)]
fn run_results_do_not_depend_on_scheduling_options(
    #[case] jobs: Option<usize>,
    #[case] parallel: bool,
) {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", RUN_DOC);
    let rec = Arc::new(Recorder::default());
    runner()
        .run_task_with_options(
            &file,
            "b",
            RunOptions {
                jobs,
                parallel,
                observer: Some(rec.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(rec.stdout("a"), ["A"]);
    assert_eq!(rec.stdout("b"), ["B"]);
}

#[test]
fn run_propagates_task_failures() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", "## f\n\n```bash\nexit 4\n```\n");
    let err = runner().run_task(&file, "f").unwrap_err();
    assert!(matches!(err.root(), Error::ExecutionFailed(4)));
    assert_eq!(err.exit_code(), 4);
    assert!(err.to_string().ends_with("(in task 'f')"), "{err}");
}

#[test]
fn run_rejects_unknown_tasks_before_running_anything() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", RUN_DOC);
    let rec = Arc::new(Recorder::default());
    let err = runner()
        .run_task_with_options(
            &file,
            "zzz",
            RunOptions {
                observer: Some(rec.clone()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::SectionNotFound(_)));
    assert!(rec.events().is_empty());
}

#[test]
fn a_raised_cancel_flag_aborts_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", RUN_DOC);
    let rec = Arc::new(Recorder::default());
    let err = runner()
        .run_task_with_options(
            &file,
            "b",
            RunOptions {
                observer: Some(rec.clone()),
                cancel: Some(Arc::new(AtomicBool::new(true))),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled));
    assert!(rec.stdout("a").is_empty());
}

#[test]
fn dry_run_does_not_execute() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let file = write(
        dir.path(),
        "t.md",
        &format!("## touch\n\n```bash\ntouch '{}'\n```\n", marker.display()),
    );
    let mut r = runner();
    r.set_dry_run(true);
    r.run_task(&file, "touch").unwrap();
    assert!(!marker.exists());
}

#[test]
fn watch_targets_cover_the_file_and_all_sources() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "t.md",
        "\
## gen

```meta
sources = [\"schema/*.json\"]
dir = \"gen\"
```

```bash
true
```

## app

```meta
depends = [\"gen\"]
sources = [\"src/**/*.rs\"]
```

```bash
true
```

## unrelated

```meta
sources = [\"nope\"]
```

```bash
true
```
",
    );
    let targets = runner().watch_targets(&file, "app").unwrap();
    // The task file itself comes first.
    assert_eq!(targets[0].patterns, [file.to_string_lossy().into_owned()]);
    assert_eq!(targets[0].dir, None);
    assert_eq!(targets.len(), 3);

    let gen_target = targets
        .iter()
        .find(|t| t.patterns == ["schema/*.json"])
        .unwrap();
    assert_eq!(gen_target.dir, Some(PathBuf::from("gen")));
    let app_target = targets
        .iter()
        .find(|t| t.patterns == ["src/**/*.rs"])
        .unwrap();
    assert_eq!(app_target.dir, None);
}

#[test]
fn watch_targets_for_a_task_without_sources_is_just_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "t.md", RUN_DOC);
    let targets = runner().watch_targets(&file, "a").unwrap();
    assert_eq!(targets.len(), 1);
}

#[test]
fn watch_targets_include_dotenv_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "t.md",
        "\
```meta
dotenv = [\".env\"]
```

## app

```meta
dotenv = [\".env.app\"]
dir = \"svc\"
```

```bash
true
```
",
    );
    let targets = runner().watch_targets(&file, "app").unwrap();
    let env_target = targets
        .iter()
        .find(|t| t.patterns == [".env", ".env.app"])
        .expect("dotenv files are watched");
    assert_eq!(env_target.dir, Some(PathBuf::from("svc")));
}

#[test]
fn dotenv_watch_targets_are_literal_paths() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "t.md",
        "## app\n\n```meta\ndotenv = [\".env[prod]\"]\n```\n\n```bash\ntrue\n```\n",
    );
    let targets = runner().watch_targets(&file, "app").unwrap();
    let env_target = targets
        .iter()
        .find(|t| t.patterns == [".env[prod]"])
        .expect("dotenv file is watched");
    assert!(env_target.literal);
    assert!(targets.iter().filter(|t| !t.literal).count() == 0);
}

#[test]
fn computed_vars_are_evaluated_again_on_every_run() {
    // `--watch` re-runs a task on one runner, so values must not go stale.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.txt");
    let src = dir.path().join("src.txt");
    let file = write(
        dir.path(),
        "t.md",
        &format!(
            "## t\n\n```meta\nvars = {{ V = {{ sh = \"cat {}\" }} }}\n```\n\n```bash\necho \"$V\" >> {}\n```\n",
            src.display(),
            out.display()
        ),
    );
    let mut r = runner();
    std::fs::write(&src, "first").unwrap();
    r.run_task(&file, "t").unwrap();
    std::fs::write(&src, "second").unwrap();
    r.run_task(&file, "t").unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "first\nsecond\n");
}

fn imported_names(main: &str, files: &[(&str, &str)]) -> Result<Vec<String>, Error> {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in files {
        write(dir.path(), name, content);
    }
    let file = write(dir.path(), "main.md", main);
    runner()
        .list_task_sections(&file)
        .map(|sections| sections.into_iter().map(|s| s.title).collect())
}

const LIB: &str = "## helper\n\n```bash\ntrue\n```\n";

#[test]
fn an_import_can_choose_its_namespace() {
    let names = imported_names(
        "```meta\nimports = [{ path = \"lib.md\", as = \"tools\" }]\n```\n\n## all\n\n```bash\ntrue\n```\n",
        &[("lib.md", LIB)],
    )
    .unwrap();
    assert_eq!(names, ["all", "tools:helper"]);
}

#[test]
fn a_flattened_import_keeps_the_task_names() {
    let names = imported_names(
        "```meta\nimports = [{ path = \"lib.md\", flatten = true }]\n```\n\n## all\n\n```bash\ntrue\n```\n",
        &[("lib.md", LIB)],
    )
    .unwrap();
    assert_eq!(names, ["all", "helper"]);
}

#[test]
fn an_optional_import_may_be_missing_but_a_plain_one_may_not() {
    let optional = "```meta\nimports = [{ path = \"none.md\", optional = true }, \"lib.md\"]\n```\n\n## all\n\n```bash\ntrue\n```\n";
    assert_eq!(
        imported_names(optional, &[("lib.md", LIB)]).unwrap(),
        ["all", "lib:helper"]
    );
    let missing = "```meta\nimports = [\"none.md\"]\n```\n\n## all\n\n```bash\ntrue\n```\n";
    assert!(imported_names(missing, &[]).is_err());
}

#[test]
fn a_glob_imports_every_match_in_order_and_skips_the_importing_file() {
    let names = imported_names(
        "```meta\nimports = [\"*.md\", \"tasks/*.md\"]\n```\n\n## all\n\n```bash\ntrue\n```\n",
        &[
            ("b.md", "## two\n\n```bash\ntrue\n```\n"),
            ("a.md", "## one\n\n```bash\ntrue\n```\n"),
            ("tasks/c.md", "## three\n\n```bash\ntrue\n```\n"),
        ],
    )
    .unwrap();
    assert_eq!(names, ["all", "a:one", "b:two", "c:three"]);
}

#[test]
fn a_glob_without_matches_imports_nothing() {
    let names = imported_names(
        "```meta\nimports = [\"none/*.md\"]\n```\n\n## all\n\n```bash\ntrue\n```\n",
        &[],
    )
    .unwrap();
    assert_eq!(names, ["all"]);
}

#[rstest]
#[case::unknown_key(
    "{ path = \"x.md\", optinal = true }",
    "unknown key `optinal` (did you mean `optional`?)"
)]
#[case::no_path("{ as = \"x\" }", "needs a `path`")]
#[case::alias_with_glob(
    "{ path = \"*.md\", as = \"x\" }",
    "`as` cannot be used with the glob '*.md'"
)]
#[case::alias_with_flatten(
    "{ path = \"x.md\", as = \"y\", flatten = true }",
    "`as` has no effect with `flatten`"
)]
#[case::wrong_type(
    "{ path = \"x.md\", optional = \"yes\" }",
    "`optional` must be true or false"
)]
#[case::not_a_path("3", "expected a path or a table")]
fn invalid_import_entries_are_explained(#[case] entry: &str, #[case] expected: &str) {
    let main = format!(
        "```meta\nimports = [{}]\n```\n\n## all\n\n```bash\ntrue\n```\n",
        entry
    );
    let err = imported_names(&main, &[]).unwrap_err().to_string();
    assert!(err.contains(expected), "{err}");
}
