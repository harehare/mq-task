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
