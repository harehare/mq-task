//! `runtimes` and `default_task` in the document-wide `meta` block.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const DOC: &str = r#"```meta
default_task = "hello"
runtimes = { shx = "sh" }
```

## hello

```shx
echo from-shx
```
"#;

fn dir_with(doc: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("tasks.md"), doc).unwrap();
    dir
}

fn cli(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn the_file_runs_alone_with_its_runtimes_and_default_task() {
    let dir = dir_with(DOC);
    let o = cli(dir.path(), &["-q"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "from-shx\n");
    assert!(cli(dir.path(), &["check"]).status.success());
}

#[test]
fn the_configuration_file_wins_over_the_document() {
    let dir = dir_with(DOC);
    // `shx` here is a command that does not exist; the document's `sh` loses.
    fs::write(
        dir.path().join("mq-task.toml"),
        "default_task = \"other\"\n[runtimes]\nshx = \"definitely-not-a-command-xyz\"\nbash = \"bash\"\n",
    )
    .unwrap();
    let o = cli(dir.path(), &["hello"]);
    assert!(!o.status.success());
    // And its `default_task` beats the document's.
    let o = cli(dir.path(), &[]);
    assert!(err(&o).contains("other"), "{}{}", out(&o), err(&o));
}

#[test]
fn the_command_line_wins_over_the_document() {
    let dir = dir_with(DOC);
    let o = cli(
        dir.path(),
        &["hello", "-r", "shx:definitely-not-a-command-xyz"],
    );
    assert!(!o.status.success());
}

#[test]
fn check_reports_an_unknown_default_task() {
    let dir =
        dir_with("```meta\ndefault_task = \"buidl\"\n```\n\n## build\n\n```bash\ntrue\n```\n");
    let o = cli(dir.path(), &["check"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        out(&o).contains("`default_task` names unknown task 'buidl' (did you mean `build`?)"),
        "{}{}",
        out(&o),
        err(&o)
    );
}
