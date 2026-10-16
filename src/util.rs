//! Small shared helpers: duration parsing and a stable content hash.

use std::time::Duration;

use crate::error::{Error, Result};

/// Parse a human-friendly duration such as `500ms`, `30s`, `2m` or `1h`.
/// A bare number is interpreted as seconds.
pub fn parse_duration(raw: &str) -> Result<Duration> {
    let s = raw.trim();
    let invalid = || {
        Error::Config(format!(
            "Invalid duration '{}'. Expected e.g. '500ms', '30s', '2m' or '1h'",
            raw
        ))
    };
    if s.is_empty() {
        return Err(invalid());
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num.parse().map_err(|_| invalid())?;
    if !value.is_finite() || value < 0.0 {
        return Err(invalid());
    }
    let seconds = match unit.trim() {
        "ms" => value / 1000.0,
        "" | "s" => value,
        "m" => value * 60.0,
        "h" => value * 3600.0,
        _ => return Err(invalid()),
    };
    // Absurdly large values overflow `Duration`; report them instead of panicking.
    Duration::try_from_secs_f64(seconds).map_err(|_| invalid())
}

/// Parse the contents of a `.env` file into `KEY=VALUE` pairs.
///
/// Supports `KEY=value`, an optional `export ` prefix, `#` comments (whole
/// lines, and after an unquoted value), single-quoted values (literal) and
/// double-quoted values (with `\n`, `\t`, `\"` and `\\` escapes). There is no
/// variable expansion. Errors name the offending line number.
pub fn parse_dotenv(content: &str) -> std::result::Result<Vec<(String, String)>, String> {
    let mut vars = Vec::new();
    for (n, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected KEY=VALUE", n + 1))?;
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            return Err(format!("line {}: invalid variable name '{}'", n + 1, key));
        }
        let value = value.trim();
        let parsed = if let Some(rest) = value.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = rest.chars();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('r') => out.push('\r'),
                        Some(other) => out.push(other),
                        None => out.push('\\'),
                    },
                    other => out.push(other),
                }
            }
            if !closed {
                return Err(format!("line {}: unterminated quote", n + 1));
            }
            out
        } else if let Some(rest) = value.strip_prefix('\'') {
            rest.split_once('\'')
                .map(|(inner, _)| inner.to_string())
                .ok_or_else(|| format!("line {}: unterminated quote", n + 1))?
        } else {
            // An unquoted value ends at whitespace followed by `#`.
            let end = value
                .char_indices()
                .find(|&(i, c)| c == '#' && value[..i].ends_with(char::is_whitespace))
                .map(|(i, _)| i)
                .unwrap_or(value.len());
            value[..end].trim_end().to_string()
        };
        vars.push((key.to_string(), parsed));
    }
    Ok(vars)
}

/// 64-bit FNV-1a hash. Unlike `DefaultHasher` its output is stable across
/// Rust releases, which matters for fingerprints persisted on disk.
#[derive(Debug, Clone)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf29ce484222325)
    }
}

impl Fnv {
    pub fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    /// Write a length-prefixed field so adjacent fields cannot run together.
    pub fn field(&mut self, bytes: &[u8]) {
        self.write(&(bytes.len() as u64).to_le_bytes());
        self.write(bytes);
    }

    pub fn finish(&self) -> String {
        format!("{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dotenv_files() {
        let content = "# comment\n\nA=1\nexport B = two words # trailing\nC=\"x\\ny \\\"q\\\" # kept\"\nD='lit # eral $X'\nE=\nF=a#b\n";
        assert_eq!(
            parse_dotenv(content).unwrap(),
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "two words".to_string()),
                ("C".to_string(), "x\ny \"q\" # kept".to_string()),
                ("D".to_string(), "lit # eral $X".to_string()),
                ("E".to_string(), String::new()),
                ("F".to_string(), "a#b".to_string()),
            ]
        );
    }

    #[test]
    fn rejects_malformed_dotenv_lines() {
        assert!(parse_dotenv("NOEQUALS").unwrap_err().contains("line 1"));
        assert!(parse_dotenv("A=1\n=x").unwrap_err().contains("line 2"));
        assert!(parse_dotenv("A B=1").unwrap_err().contains("invalid"));
        assert!(
            parse_dotenv("A=\"open")
                .unwrap_err()
                .contains("unterminated")
        );
        assert!(
            parse_dotenv("A='open")
                .unwrap_err()
                .contains("unterminated")
        );
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("5").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
    }

    #[test]
    fn rejects_bad_durations() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("-1s").is_err());
    }

    #[test]
    fn fnv_is_deterministic() {
        let mut a = Fnv::default();
        a.field(b"hello");
        let mut b = Fnv::default();
        b.field(b"hello");
        assert_eq!(a.finish(), b.finish());
        let mut c = Fnv::default();
        c.field(b"hellp");
        assert_ne!(a.finish(), c.finish());
    }
}
