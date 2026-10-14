//! Tiny condition language used by the `if` key of a task's `meta` block.
//!
//! ```text
//! expr  := term ("||" term)*
//! term  := factor ("&&" factor)*
//! factor:= "!" factor | atom
//! atom  := env.NAME                  # set and not empty/"0"/"false"
//!        | env.NAME == "value"       # also !=
//!        | os == "linux"             # also !=; linux | macos | windows
//!        | exists("path")
//!        | which("command")
//!        | true | false
//! ```

use std::collections::HashMap;
use std::path::Path;

use crate::error::{Error, Result};

/// Values a condition is evaluated against.
pub struct CondContext<'a> {
    /// Task-level environment (document/task `meta`, params, `--env`).
    pub env: &'a HashMap<String, String>,
    /// Directory `exists(...)` paths are resolved against.
    pub dir: Option<&'a Path>,
}

impl CondContext<'_> {
    fn env_var(&self, name: &str) -> Option<String> {
        self.env
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
    }
}

/// Evaluate `expr`, returning an error for malformed expressions.
pub fn evaluate(expr: &str, ctx: &CondContext) -> Result<bool> {
    for alt in split_top(expr, "||") {
        let mut all = true;
        for term in split_top(&alt, "&&") {
            if !eval_factor(term.trim(), ctx, expr)? {
                all = false;
                break;
            }
        }
        if all {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Split on `sep`, ignoring occurrences inside double quotes.
fn split_top(s: &str, sep: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let bytes = s.as_bytes();
    let sep_b = sep.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            in_quote = !in_quote;
        }
        if !in_quote && bytes[i..].starts_with(sep_b) {
            parts.push(std::mem::take(&mut cur));
            i += sep_b.len();
            continue;
        }
        // Push the full UTF-8 char starting at i.
        let ch = s[i..].chars().next().unwrap();
        cur.push(ch);
        i += ch.len_utf8();
    }
    parts.push(cur);
    parts
}

fn bad(expr: &str, why: &str) -> Error {
    Error::Config(format!("Invalid `if` condition '{}': {}", expr, why))
}

fn eval_factor(f: &str, ctx: &CondContext, whole: &str) -> Result<bool> {
    if let Some(rest) = f.strip_prefix('!')
        && !rest.starts_with('=')
    {
        return Ok(!eval_factor(rest.trim(), ctx, whole)?);
    }
    match f {
        "true" => return Ok(true),
        "false" => return Ok(false),
        "" => return Err(bad(whole, "empty term")),
        _ => {}
    }

    if let Some(arg) = call_arg(f, "exists") {
        let p = unquote(arg).ok_or_else(|| bad(whole, "exists() expects a quoted path"))?;
        let path = match ctx.dir {
            Some(d) => d.join(p),
            None => p.into(),
        };
        return Ok(path.exists());
    }
    if let Some(arg) = call_arg(f, "which") {
        let c = unquote(arg).ok_or_else(|| bad(whole, "which() expects a quoted command"))?;
        return Ok(which::which(c).is_ok());
    }

    // lhs (==|!=) "value"
    for op in ["==", "!="] {
        if let Some((lhs, rhs)) = f.split_once(op) {
            let want = unquote(rhs.trim())
                .ok_or_else(|| bad(whole, "right-hand side must be a quoted string"))?;
            let have = operand(lhs.trim(), ctx, whole)?;
            let eq = have.as_deref() == Some(want.as_str());
            return Ok(if op == "==" { eq } else { !eq });
        }
    }

    // bare `env.NAME` truthiness
    if f.starts_with("env.") {
        let v = operand(f, ctx, whole)?;
        return Ok(match v {
            Some(v) => !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false")),
            None => false,
        });
    }

    Err(bad(whole, &format!("unrecognized term '{}'", f)))
}

fn operand(s: &str, ctx: &CondContext, whole: &str) -> Result<Option<String>> {
    if s == "os" {
        return Ok(Some(
            match std::env::consts::OS {
                "macos" => "macos",
                "windows" => "windows",
                other => other,
            }
            .to_string(),
        ));
    }
    if let Some(name) = s.strip_prefix("env.")
        && !name.is_empty()
    {
        return Ok(ctx.env_var(name));
    }
    Err(bad(whole, &format!("unknown operand '{}'", s)))
}

fn call_arg<'a>(f: &'a str, name: &str) -> Option<&'a str> {
    f.strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')
        .map(str::trim)
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    s.strip_prefix('"')?.strip_suffix('"').map(str::to_string)
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
    fn env_truthiness() {
        assert!(eval("env.CI", &[("CI", "1")]).unwrap());
        assert!(!eval("env.CI", &[("CI", "0")]).unwrap());
        assert!(!eval("env.CI", &[("CI", "false")]).unwrap());
        assert!(!eval("env.CI", &[("CI", "")]).unwrap());
        assert!(!eval("env.MQ_TASK_SURELY_UNSET_VAR", &[]).unwrap());
    }

    #[test]
    fn env_comparison() {
        assert!(eval(r#"env.REGION == "eu""#, &[("REGION", "eu")]).unwrap());
        assert!(!eval(r#"env.REGION == "eu""#, &[("REGION", "us")]).unwrap());
        assert!(eval(r#"env.REGION != "eu""#, &[("REGION", "us")]).unwrap());
    }

    #[test]
    fn boolean_operators() {
        let env = [("A", "1"), ("B", "")];
        assert!(eval("env.A && !env.B", &env).unwrap());
        assert!(eval("env.B || env.A", &env).unwrap());
        assert!(!eval("env.A && env.B", &env).unwrap());
        assert!(eval("true || false && false", &env).unwrap());
    }

    #[test]
    fn os_and_which() {
        let os = std::env::consts::OS;
        assert!(eval(&format!(r#"os == "{}""#, os), &[]).unwrap());
        assert!(!eval(r#"which("definitely-not-a-real-command-xyz")"#, &[]).unwrap());
        assert!(eval(r#"exists("Cargo.toml")"#, &[]).unwrap());
    }

    #[test]
    fn quoted_operators_are_not_split() {
        assert!(eval(r#"env.X == "a||b""#, &[("X", "a||b")]).unwrap());
    }

    #[test]
    fn errors_on_garbage() {
        assert!(eval("nonsense", &[]).is_err());
        assert!(eval("env.X ==", &[("X", "1")]).is_err());
        assert!(eval("", &[]).is_err());
    }
}
