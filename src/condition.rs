//! Conditions for the `if` key of a task's `meta` block.
//!
//! A condition is an mq expression that evaluates to a boolean, with a few
//! host functions registered for the things a task runner cares about:
//!
//! ```text
//! flag("CI")                   # set and not empty/"0"/"false"
//! getenv("REGION") == "eu"     # string, or none when unset
//! os() == "linux"              # linux | macos | windows
//! exists("path")               # relative to the task's working directory
//! which("command")
//! flag("CI") && !exists("dist") || os() == "macos"
//! ```
//!
//! Environment lookups see the task-level environment first (document/task
//! `meta`, params, `--env`) and fall back to the process environment. mq's
//! own `env()` is deliberately not wired to that, so use `getenv`/`flag`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mq_lang::{DefaultEngine, RuntimeValue};

use crate::error::{Error, Result};

/// Values a condition is evaluated against.
pub struct CondContext<'a> {
    /// Task-level environment (document/task `meta`, params, `--env`).
    pub env: &'a HashMap<String, String>,
    /// Directory `exists(...)` paths are resolved against.
    pub dir: Option<&'a Path>,
}

fn lookup(env: &HashMap<String, String>, name: &str) -> Option<String> {
    env.get(name).cloned().or_else(|| std::env::var(name).ok())
}

fn is_truthy(value: &str) -> bool {
    !(value.is_empty() || value == "0" || value.eq_ignore_ascii_case("false"))
}

/// Evaluate `expr`, returning an error for malformed expressions.
pub fn evaluate(expr: &str, ctx: &CondContext) -> Result<bool> {
    let bad = |why: String| {
        Error::Config(format!(
            "Invalid `if` condition '{}': {}",
            expr,
            why.lines().next().unwrap_or("").trim()
        ))
    };
    if expr.trim().is_empty() {
        return Err(bad("empty expression".into()));
    }

    let mut engine = DefaultEngine::default();
    engine.load_builtin_module();

    let env = ctx.env.clone();
    engine.register_fn("getenv", {
        let env = env.clone();
        move |name: String| Ok(lookup(&env, &name))
    });
    engine.register_fn("flag", move |name: String| {
        Ok(lookup(&env, &name).is_some_and(|v| is_truthy(&v)))
    });
    engine.register_fn("os", || Ok(std::env::consts::OS.to_string()));
    let dir: Option<PathBuf> = ctx.dir.map(Path::to_path_buf);
    engine.register_fn("exists", move |path: String| {
        let path = match &dir {
            Some(d) => d.join(path),
            None => path.into(),
        };
        Ok(path.exists())
    });
    engine.register_fn("which", |cmd: String| Ok(which::which(cmd).is_ok()));

    // The input is irrelevant, but an empty one would skip evaluation.
    let input = mq_lang::parse_text_input("-").map_err(|e| bad(e.to_string()))?;
    let result = engine
        .eval(expr, input.into_iter())
        .map_err(|e| bad(e.to_string()))?;

    match result.into_iter().next() {
        Some(RuntimeValue::Boolean(b)) => Ok(b),
        Some(RuntimeValue::None) => Ok(false),
        Some(other) => Err(bad(format!(
            "must evaluate to true or false, got {}",
            other
        ))),
        None => Err(bad("empty expression".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(expr: &str, env: &[(&str, &str)]) -> Result<bool> {
        let map: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        evaluate(
            expr,
            &CondContext {
                env: &map,
                dir: None,
            },
        )
    }

    #[test]
    fn flag_truthiness() {
        assert!(eval(r#"flag("CI")"#, &[("CI", "1")]).unwrap());
        assert!(!eval(r#"flag("CI")"#, &[("CI", "0")]).unwrap());
        assert!(!eval(r#"flag("CI")"#, &[("CI", "false")]).unwrap());
        assert!(!eval(r#"flag("CI")"#, &[("CI", "")]).unwrap());
        assert!(!eval(r#"flag("MQ_TASK_SURELY_UNSET_VAR")"#, &[]).unwrap());
    }

    #[test]
    fn getenv_comparison() {
        assert!(eval(r#"getenv("REGION") == "eu""#, &[("REGION", "eu")]).unwrap());
        assert!(!eval(r#"getenv("REGION") == "eu""#, &[("REGION", "us")]).unwrap());
        assert!(eval(r#"getenv("REGION") != "eu""#, &[("REGION", "us")]).unwrap());
        assert!(!eval(r#"getenv("MQ_TASK_SURELY_UNSET_VAR") == "eu""#, &[]).unwrap());
    }

    #[test]
    fn task_env_wins_over_process_env() {
        // PATH is always set in the process; the task value must shadow it.
        assert!(eval(r#"getenv("PATH") == "mine""#, &[("PATH", "mine")]).unwrap());
    }

    #[test]
    fn boolean_operators() {
        let env = [("A", "1"), ("B", "")];
        assert!(eval(r#"flag("A") && !flag("B")"#, &env).unwrap());
        assert!(eval(r#"flag("B") || flag("A")"#, &env).unwrap());
        assert!(!eval(r#"flag("A") && flag("B")"#, &env).unwrap());
        assert!(eval("true || false && false", &env).unwrap());
        assert!(!eval(r#"(flag("A") || true) && flag("B")"#, &env).unwrap());
    }

    #[test]
    fn os_and_which() {
        let os = std::env::consts::OS;
        assert!(eval(&format!(r#"os() == "{}""#, os), &[]).unwrap());
        assert!(!eval(r#"which("definitely-not-a-real-command-xyz")"#, &[]).unwrap());
        assert!(eval(r#"exists("Cargo.toml")"#, &[]).unwrap());
    }

    #[test]
    fn quoted_operators_are_plain_strings() {
        assert!(eval(r#"getenv("X") == "a||b""#, &[("X", "a||b")]).unwrap());
    }

    #[test]
    fn errors_on_garbage() {
        assert!(eval("nonsense", &[]).is_err());
        assert!(eval(r#"getenv("X") =="#, &[("X", "1")]).is_err());
        assert!(eval("", &[]).is_err());
        assert!(eval(r#"getenv("X")"#, &[("X", "1")]).is_err());
    }
}
