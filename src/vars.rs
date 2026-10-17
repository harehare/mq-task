//! `vars`: named values for tasks, optionally computed by a shell command.
//!
//! ```toml
//! vars = { NAME = "app", VERSION = { sh = "git describe --tags" }, IMAGE = "${NAME}:${VERSION}" }
//! ```
//!
//! Values reach the task as environment variables. `${NAME}` inside a value
//! (or an `sh` command) refers to another variable; `$${` writes a literal
//! `${`. Shell commands run once per run for the same command and directory.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The value of a variable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VarValue {
    /// A fixed string; `${NAME}` references are expanded.
    Literal(String),
    /// The output of a shell command, with trailing newlines removed.
    Shell(String),
}

/// A variable declared in a `vars` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VarDef {
    pub name: String,
    pub value: VarValue,
}

/// Results of shell variables for this run, keyed by directory and command,
/// so a command used by several tasks runs once.
pub type ShellCache = Mutex<HashMap<(Option<PathBuf>, String), String>>;

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Read a `vars` table from a `meta` block.
pub fn parse_table(context: &str, table: &toml::Table) -> Result<Vec<VarDef>> {
    table
        .iter()
        .map(|(name, value)| {
            let invalid =
                |why: &str| Error::Invalid(format!("{}: var `{}` {}", context, name, why));
            if !is_ident(name) {
                return Err(invalid("is not a valid name (letters, digits and `_`)"));
            }
            let value = match value {
                toml::Value::String(s) => VarValue::Literal(s.clone()),
                toml::Value::Integer(i) => VarValue::Literal(i.to_string()),
                toml::Value::Float(f) => VarValue::Literal(f.to_string()),
                toml::Value::Boolean(b) => VarValue::Literal(b.to_string()),
                toml::Value::Table(t) => match (t.len(), t.get("sh")) {
                    (1, Some(toml::Value::String(cmd))) if !cmd.trim().is_empty() => {
                        VarValue::Shell(cmd.clone())
                    }
                    _ => return Err(invalid("must be `{ sh = \"command\" }` when it is a table")),
                },
                _ => {
                    return Err(invalid(
                        "must be a string, number, boolean or `{ sh = \"command\" }`",
                    ));
                }
            };
            Ok(VarDef {
                name: name.clone(),
                value,
            })
        })
        .collect()
}

/// Walk `s` and call `on_ref(name, start, end)` for each `${NAME}`; `$${` is
/// an escape and `${...}` that is not a plain name is left alone.
fn scan(s: &str, mut on_ref: impl FnMut(&str, usize, usize)) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if s[i..].starts_with("$${") {
            i += 3;
        } else if s[i..].starts_with("${")
            && let Some(close) = s[i + 2..].find('}')
            && is_ident(&s[i + 2..i + 2 + close])
        {
            on_ref(&s[i + 2..i + 2 + close], i, i + 3 + close);
            i += 3 + close;
        } else {
            i += s[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
}

/// Names referenced as `${NAME}` in `s`.
fn references(s: &str) -> Vec<String> {
    let mut names = Vec::new();
    scan(s, |name, _, _| names.push(name.to_string()));
    names
}

/// Replace `${NAME}` using `lookup`. A name `lookup` does not know is an
/// error when `strict`, else the reference is kept as it was written.
fn interpolate(
    s: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    strict: bool,
) -> std::result::Result<String, String> {
    let mut found: Vec<(usize, usize, String)> = Vec::new();
    let mut unknown = None;
    scan(s, |name, start, end| match lookup(name) {
        Some(value) => found.push((start, end, value)),
        None if strict => {
            unknown.get_or_insert_with(|| name.to_string());
        }
        None => {}
    });
    if let Some(name) = unknown {
        return Err(name);
    }
    let mut out = String::new();
    let mut at = 0;
    for (start, end, value) in found {
        out.push_str(&s[at..start]);
        out.push_str(&value);
        at = end;
    }
    out.push_str(&s[at..]);
    Ok(out.replace("$${", "${"))
}

/// Run `command` with `sh -c` (`cmd /C` on Windows) and return its output.
fn run_shell(
    name: &str,
    command: &str,
    env: &[(String, String)],
    dir: Option<&Path>,
) -> Result<String> {
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    #[cfg(not(unix))]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    cmd.envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let out = cmd.output().map_err(|e| {
        Error::Variable(
            name.to_string(),
            format!("could not run `{}`: {}", command, e),
        )
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = stderr.lines().next().unwrap_or("").trim();
        return Err(Error::Variable(
            name.to_string(),
            format!(
                "`{}` exited with {}{}{}",
                command,
                out.status
                    .code()
                    .map_or("a signal".to_string(), |c| format!("code {}", c)),
                if detail.is_empty() { "" } else { ": " },
                detail
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_string())
}

/// Everything `resolve` needs besides the definitions.
pub struct ResolveContext<'a> {
    /// Values a `${NAME}` can refer to besides other vars, in increasing
    /// precedence (`.env` files, loop variables, `env`, `--env`). The process
    /// environment is consulted last.
    pub base: &'a [(String, String)],
    /// Names set by `env` or `--env`: they win over a var of the same name,
    /// which is then not evaluated at all.
    pub fixed: &'a [String],
    /// Where shell commands run.
    pub dir: Option<&'a Path>,
    /// Do not run commands; show them instead.
    pub dry_run: bool,
    pub cache: &'a ShellCache,
}

/// Evaluate `defs` (later definitions of a name replace earlier ones) and
/// return the resulting variables. A var may refer to another one; a
/// reference to its own name means the outer value of that name.
pub fn resolve(defs: &[VarDef], ctx: &ResolveContext) -> Result<Vec<(String, String)>> {
    let mut by_name: Vec<&VarDef> = Vec::new();
    for def in defs {
        by_name.retain(|d| d.name != def.name);
        by_name.push(def);
    }
    by_name.retain(|d| !ctx.fixed.contains(&d.name));
    let index: HashMap<&str, usize> = by_name
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i))
        .collect();

    // Order so that a var comes after the vars it refers to.
    let text = |d: &VarDef| match &d.value {
        VarValue::Literal(s) | VarValue::Shell(s) => s.clone(),
    };
    let mut order: Vec<usize> = Vec::new();
    let mut state = vec![0u8; by_name.len()]; // 0 new, 1 visiting, 2 done
    fn visit(
        i: usize,
        by_name: &[&VarDef],
        index: &HashMap<&str, usize>,
        text: &dyn Fn(&VarDef) -> String,
        state: &mut [u8],
        order: &mut Vec<usize>,
        path: &mut Vec<String>,
    ) -> Result<()> {
        match state[i] {
            2 => return Ok(()),
            1 => {
                path.push(by_name[i].name.clone());
                return Err(Error::Variable(
                    by_name[i].name.clone(),
                    format!("circular reference: {}", path.join(" -> ")),
                ));
            }
            _ => {}
        }
        state[i] = 1;
        path.push(by_name[i].name.clone());
        for r in references(&text(by_name[i])) {
            if r != by_name[i].name
                && let Some(&j) = index.get(r.as_str())
            {
                visit(j, by_name, index, text, state, order, path)?;
            }
        }
        path.pop();
        state[i] = 2;
        order.push(i);
        Ok(())
    }
    for i in 0..by_name.len() {
        visit(
            i,
            &by_name,
            &index,
            &text,
            &mut state,
            &mut order,
            &mut Vec::new(),
        )?;
    }

    let mut resolved: Vec<(String, String)> = Vec::new();
    for i in order {
        let def = by_name[i];
        let lookup = |name: &str| -> Option<String> {
            if name != def.name
                && let Some((_, v)) = resolved.iter().find(|(k, _)| k == name)
            {
                return Some(v.clone());
            }
            ctx.base
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var(name).ok())
        };
        let value = match &def.value {
            VarValue::Literal(s) => interpolate(s, &lookup, true).map_err(|unknown| {
                Error::Variable(
                    def.name.clone(),
                    format!("unknown variable `${{{}}}`", unknown),
                )
            })?,
            VarValue::Shell(cmd) => {
                // Unknown names stay: the shell may define them itself.
                let cmd = interpolate(cmd, &lookup, false).unwrap_or_else(|_| cmd.clone());
                if ctx.dry_run {
                    format!("<sh: {}>", cmd)
                } else {
                    let key = (ctx.dir.map(Path::to_path_buf), cmd.clone());
                    let mut cache = ctx.cache.lock().unwrap_or_else(|e| e.into_inner());
                    match cache.get(&key) {
                        Some(hit) => hit.clone(),
                        None => {
                            let mut env: Vec<(String, String)> = ctx.base.to_vec();
                            env.extend(resolved.iter().cloned());
                            let out = run_shell(&def.name, &cmd, &env, ctx.dir)?;
                            cache.insert(key, out.clone());
                            out
                        }
                    }
                }
            }
        };
        resolved.push((def.name.clone(), value));
    }

    // Report in the order the vars were declared.
    let declared: HashSet<&str> = by_name.iter().map(|d| d.name.as_str()).collect();
    debug_assert_eq!(declared.len(), resolved.len());
    resolved.sort_by_key(|(name, _)| by_name.iter().position(|d| d.name == *name));
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(name: &str, value: &str) -> VarDef {
        VarDef {
            name: name.into(),
            value: VarValue::Literal(value.into()),
        }
    }

    fn sh(name: &str, cmd: &str) -> VarDef {
        VarDef {
            name: name.into(),
            value: VarValue::Shell(cmd.into()),
        }
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn run(
        defs: &[VarDef],
        base: &[(&str, &str)],
        fixed: &[&str],
    ) -> Result<Vec<(String, String)>> {
        let cache = ShellCache::default();
        resolve(
            defs,
            &ResolveContext {
                base: &pairs(base),
                fixed: &fixed.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                dir: None,
                dry_run: false,
                cache: &cache,
            },
        )
    }

    fn value(vars: &[(String, String)], name: &str) -> String {
        vars.iter().find(|(k, _)| k == name).unwrap().1.clone()
    }

    #[test]
    fn names_must_be_identifiers() {
        for good in ["A", "_x", "Version2", "a_b"] {
            assert!(is_ident(good), "{good}");
        }
        for bad in ["", "1a", "a-b", "a b", "a.b", "日本"] {
            assert!(!is_ident(bad), "{bad}");
        }
    }

    #[test]
    fn parses_literals_numbers_booleans_and_shell_values() {
        let table: toml::Table =
            toml::from_str("S = \"text\"\nI = 3\nF = 1.5\nB = true\nC = { sh = \"echo hi\" }\n")
                .unwrap();
        let mut vars = parse_table("ctx", &table).unwrap();
        vars.sort_by(|a, b| a.name.cmp(&b.name));
        let by = |n: &str| vars.iter().find(|v| v.name == n).unwrap().value.clone();
        assert_eq!(by("S"), VarValue::Literal("text".into()));
        assert_eq!(by("I"), VarValue::Literal("3".into()));
        assert_eq!(by("F"), VarValue::Literal("1.5".into()));
        assert_eq!(by("B"), VarValue::Literal("true".into()));
        assert_eq!(by("C"), VarValue::Shell("echo hi".into()));
    }

    #[test]
    fn rejects_invalid_names_and_values() {
        for src in [
            "\"a-b\" = 1",
            "A = [1, 2]",
            "A = { cmd = \"x\" }",
            "A = { sh = 3 }",
            "A = { sh = \"\" }",
            "A = { sh = \"x\", extra = 1 }",
        ] {
            let table: toml::Table = toml::from_str(src).unwrap();
            let err = parse_table("ctx", &table).unwrap_err().to_string();
            assert!(err.contains("ctx: var `"), "{src}: {err}");
        }
    }

    #[test]
    fn references_skip_escapes_and_non_names() {
        assert_eq!(references("${A} and ${B}"), vec!["A", "B"]);
        assert!(references("$${A}").is_empty());
        assert!(references("${x:-default}").is_empty());
        assert!(references("${}").is_empty());
        assert!(references("$A and $(cmd)").is_empty());
        assert!(references("unterminated ${A").is_empty());
        assert_eq!(references("日本語 ${A}"), vec!["A"]);
    }

    #[test]
    fn interpolation_replaces_known_names_only_in_braces() {
        let lookup = |n: &str| (n == "A").then(|| "1".to_string());
        assert_eq!(interpolate("x${A}y${A}", &lookup, true).unwrap(), "x1y1");
        assert_eq!(
            interpolate("$A ${x:-d}", &lookup, true).unwrap(),
            "$A ${x:-d}"
        );
        assert_eq!(interpolate("$${A}", &lookup, true).unwrap(), "${A}");
        assert_eq!(interpolate("${B}", &lookup, false).unwrap(), "${B}");
        assert_eq!(interpolate("${B} ${A}", &lookup, true).unwrap_err(), "B");
        assert_eq!(interpolate("日本${A}語", &lookup, true).unwrap(), "日本1語");
    }

    #[test]
    fn vars_can_refer_to_each_other_in_any_order() {
        let vars = run(
            &[
                lit("IMAGE", "${NAME}:${VERSION}"),
                lit("VERSION", "v${MAJOR}.0"),
                lit("MAJOR", "2"),
                lit("NAME", "app"),
            ],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(value(&vars, "IMAGE"), "app:v2.0");
        // Reported in declaration order.
        let names: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, vec!["IMAGE", "VERSION", "MAJOR", "NAME"]);
    }

    #[test]
    fn a_later_definition_replaces_an_earlier_one() {
        let vars = run(&[lit("A", "first"), lit("A", "second")], &[], &[]).unwrap();
        assert_eq!(vars, pairs(&[("A", "second")]));
    }

    #[test]
    fn a_var_naming_itself_extends_the_outer_value() {
        let vars = run(
            &[lit("PATH_X", "${PATH_X}:/opt")],
            &[("PATH_X", "/bin")],
            &[],
        )
        .unwrap();
        assert_eq!(value(&vars, "PATH_X"), "/bin:/opt");
        let err = run(
            &[lit("MQ_TASK_SURELY_UNSET", "${MQ_TASK_SURELY_UNSET}")],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown variable"), "{err}");
    }

    #[test]
    fn references_fall_back_to_base_then_the_process_environment() {
        let vars = run(
            &[lit("A", "${FROM_BASE}-${HOME}")],
            &[("FROM_BASE", "b"), ("FROM_BASE", "later")],
            &[],
        )
        .unwrap();
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(value(&vars, "A"), format!("later-{home}"));
        }
    }

    #[test]
    fn circular_references_are_reported_with_the_path() {
        let err = run(
            &[lit("A", "${B}"), lit("B", "${C}"), lit("C", "${A}")],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("circular reference"), "{err}");
        assert!(err.contains("A -> B -> C -> A"), "{err}");
    }

    #[test]
    fn unknown_references_name_the_variable_and_the_unknown_name() {
        let err = run(&[lit("A", "${MQ_TASK_NOPE}")], &[], &[])
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Variable 'A': unknown variable `${MQ_TASK_NOPE}`");
    }

    #[test]
    fn fixed_names_win_and_are_not_evaluated() {
        let cache = ShellCache::default();
        let vars = resolve(
            &[sh("VERSION", "exit 9"), lit("TAG", "v${VERSION}")],
            &ResolveContext {
                base: &pairs(&[("VERSION", "7")]),
                fixed: &["VERSION".to_string()],
                dir: None,
                dry_run: false,
                cache: &cache,
            },
        )
        .unwrap();
        // The failing command never ran, and TAG sees the fixed value.
        assert_eq!(vars, pairs(&[("TAG", "v7")]));
    }

    #[cfg(unix)]
    #[test]
    fn shell_values_are_trimmed_and_can_use_other_vars_and_the_base_env() {
        let vars = run(
            &[
                lit("GREETING", "hello"),
                sh("A", "printf '%s\\n\\n' \"${GREETING} $FROM_BASE\""),
                sh("B", "echo ${A}-via-env"),
            ],
            &[("FROM_BASE", "world")],
            &[],
        )
        .unwrap();
        assert_eq!(value(&vars, "A"), "hello world");
        assert_eq!(value(&vars, "B"), "hello world-via-env");
    }

    #[cfg(unix)]
    #[test]
    fn shell_commands_that_name_unknown_variables_leave_them_to_the_shell() {
        let vars = run(
            &[sh("A", "for i in 1 2; do printf \"${i}\"; done")],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(value(&vars, "A"), "12");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_command_reports_its_code_and_first_stderr_line() {
        let err = run(
            &[sh("A", "echo first >&2; echo second >&2; exit 3")],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            err,
            "Variable 'A': `echo first >&2; echo second >&2; exit 3` exited with code 3: first"
        );
        let err = run(&[sh("B", "exit 1")], &[], &[]).unwrap_err().to_string();
        assert_eq!(err, "Variable 'B': `exit 1` exited with code 1");
    }

    #[cfg(unix)]
    #[test]
    fn the_same_command_runs_once_per_cache() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("runs");
        let cmd = format!("echo x >> {}; echo value", log.display());
        let cache = ShellCache::default();
        let ctx = |cache: &ShellCache| {
            resolve(
                &[sh("A", &cmd), sh("B", &cmd)],
                &ResolveContext {
                    base: &[],
                    fixed: &[],
                    dir: None,
                    dry_run: false,
                    cache,
                },
            )
            .unwrap()
        };
        assert_eq!(value(&ctx(&cache), "A"), "value");
        assert_eq!(value(&ctx(&cache), "B"), "value");
        let runs = std::fs::read_to_string(&log).unwrap().lines().count();
        assert_eq!(runs, 1, "cached after the first run");

        // A fresh cache (the next run) evaluates again.
        ctx(&ShellCache::default());
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn commands_run_in_the_given_directory() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ShellCache::default();
        let vars = resolve(
            &[sh("WHERE", "pwd")],
            &ResolveContext {
                base: &[],
                fixed: &[],
                dir: Some(dir.path()),
                dry_run: false,
                cache: &cache,
            },
        )
        .unwrap();
        let expected = dir.path().canonicalize().unwrap();
        assert_eq!(
            Path::new(&value(&vars, "WHERE")).canonicalize().unwrap(),
            expected
        );
    }

    #[test]
    fn a_dry_run_shows_the_command_instead_of_running_it() {
        let cache = ShellCache::default();
        let vars = resolve(
            &[lit("N", "app"), sh("V", "exit 9 # ${N}")],
            &ResolveContext {
                base: &[],
                fixed: &[],
                dir: None,
                dry_run: true,
                cache: &cache,
            },
        )
        .unwrap();
        assert_eq!(value(&vars, "V"), "<sh: exit 9 # app>");
    }
}
