//! Runtime commands are split like a shell would: quoted paths may hold spaces.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn run_with_runtime(command_for: impl Fn(&str) -> String) -> (bool, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let tools = dir.path().join("my tools");
    std::fs::create_dir(&tools).unwrap();
    let script = tools.join("runner.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat > /dev/null\necho \"quoted-ok $*\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.path().join("tasks.md"), "## t\n\n```x\nanything\n```\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mq-task"))
        .args(["-f", "tasks.md", "-r"])
        .arg(format!("x:{}", command_for(script.to_str().unwrap())))
        .arg("t")
        .current_dir(dir.path())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_double_quoted_path_with_spaces_runs() {
    let (ok, stdout, stderr) = run_with_runtime(|script| format!("\"{}\" -u", script));
    assert!(ok, "{stderr}");
    assert!(stdout.contains("quoted-ok -u"), "{stdout}");
}

#[test]
fn a_backslash_escaped_space_runs() {
    let (ok, stdout, stderr) = run_with_runtime(|script| script.replace(' ', "\\ "));
    assert!(ok, "{stderr}");
    assert!(stdout.contains("quoted-ok"), "{stdout}");
}

#[test]
fn an_unterminated_quote_is_a_clear_error() {
    let (ok, _, stderr) = run_with_runtime(|script| format!("\"{}", script));
    assert!(!ok);
    assert!(stderr.contains("unterminated quote"), "{stderr}");
}
