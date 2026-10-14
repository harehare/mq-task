//! `util`: duration parsing and the stable FNV-1a hash.

use std::time::Duration;

use mq_task::util::{Fnv, parse_duration};
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case("500ms", Duration::from_millis(500))]
#[case("0ms", Duration::ZERO)]
#[case("0", Duration::ZERO)]
#[case("30s", Duration::from_secs(30))]
#[case("2m", Duration::from_secs(120))]
#[case("1h", Duration::from_secs(3600))]
#[case("5", Duration::from_secs(5))]
#[case("1.5s", Duration::from_millis(1500))]
#[case(".5s", Duration::from_millis(500))]
#[case("5.", Duration::from_secs(5))]
#[case("0.25m", Duration::from_secs(15))]
#[case("1.5h", Duration::from_secs(5400))]
#[case("  30s  ", Duration::from_secs(30))]
#[case("1 s", Duration::from_secs(1))]
#[case("007s", Duration::from_secs(7))]
#[case("1500ms", Duration::from_millis(1500))]
fn parses_valid_durations(#[case] raw: &str, #[case] expected: Duration) {
    assert_eq!(parse_duration(raw).unwrap(), expected);
}

#[rstest]
#[case::empty("")]
#[case::blank("   ")]
#[case::no_number("abc")]
#[case::unit_only("s")]
#[case::unknown_unit("10x")]
#[case::days("1d")]
#[case::uppercase_unit("30S")]
#[case::uppercase_ms("30MS")]
#[case::negative("-1s")]
#[case::plus_sign("+1s")]
#[case::two_dots("1.2.3s")]
#[case::lone_dot(".")]
#[case::exponent("1e3")]
#[case::trailing_garbage("10s5")]
#[case::composite("1m30s")]
#[case::nan("NaN")]
#[case::inf("inf")]
#[case::overflow("99999999999999999999999999999h")]
#[case::overflow_seconds("999999999999999999999999999999999999")]
fn rejects_invalid_durations(#[case] raw: &str) {
    let err = parse_duration(raw).unwrap_err().to_string();
    assert!(err.contains("Invalid duration"), "{err}");
    assert!(err.contains(raw), "message should quote the input: {err}");
}

proptest! {
    #[test]
    fn whole_numbers_scale_by_unit(n in 0u32..1_000_000) {
        let n64 = u64::from(n);
        prop_assert_eq!(parse_duration(&format!("{n}ms")).unwrap(), Duration::from_millis(n64));
        prop_assert_eq!(parse_duration(&format!("{n}s")).unwrap(), Duration::from_secs(n64));
        prop_assert_eq!(parse_duration(&n.to_string()).unwrap(), Duration::from_secs(n64));
        prop_assert_eq!(parse_duration(&format!("{n}m")).unwrap(), Duration::from_secs(n64 * 60));
        prop_assert_eq!(parse_duration(&format!("{n}h")).unwrap(), Duration::from_secs(n64 * 3600));
    }

    #[test]
    fn units_are_consistent_with_each_other(n in 0u32..10_000) {
        let ms = parse_duration(&format!("{}ms", u64::from(n) * 1000)).unwrap();
        let s = parse_duration(&format!("{n}s")).unwrap();
        prop_assert_eq!(ms, s);
        let m = parse_duration(&format!("{}m", n)).unwrap();
        let s60 = parse_duration(&format!("{}s", u64::from(n) * 60)).unwrap();
        prop_assert_eq!(m, s60);
    }

    #[test]
    fn surrounding_whitespace_is_ignored(n in 0u32..10_000, pre in " {0,3}", post in " {0,3}") {
        let plain = parse_duration(&format!("{n}s")).unwrap();
        prop_assert_eq!(parse_duration(&format!("{pre}{n}s{post}")).unwrap(), plain);
    }

    #[test]
    fn never_panics_on_arbitrary_input(raw in ".{0,40}") {
        let _ = parse_duration(&raw);
    }

    #[test]
    fn never_panics_on_digit_heavy_input(raw in "[0-9.]{0,40}(ms|s|m|h)?") {
        let _ = parse_duration(&raw);
    }

    #[test]
    fn longer_values_are_never_shorter(a in 0u32..100_000, b in 0u32..100_000) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let short = parse_duration(&format!("{lo}ms")).unwrap();
        let long = parse_duration(&format!("{hi}ms")).unwrap();
        prop_assert!(short <= long);
    }
}

fn hash_fields(fields: &[&[u8]]) -> String {
    let mut h = Fnv::default();
    for f in fields {
        h.field(f);
    }
    h.finish()
}

#[rstest]
// Known FNV-1a 64-bit vectors (the empty input is the offset basis).
#[case(b"", "cbf29ce484222325")]
#[case(b"a", "af63dc4c8601ec8c")]
#[case(b"foobar", "85944171f73967e8")]
fn fnv_matches_reference_vectors(#[case] input: &[u8], #[case] expected: &str) {
    let mut h = Fnv::default();
    h.write(input);
    assert_eq!(h.finish(), expected);
}

#[test]
fn fnv_fields_are_length_prefixed() {
    // Without the prefix these would hash identically.
    assert_ne!(hash_fields(&[b"ab", b"c"]), hash_fields(&[b"a", b"bc"]));
    assert_ne!(hash_fields(&[b"", b"a"]), hash_fields(&[b"a", b""]));
    assert_ne!(hash_fields(&[b"a"]), hash_fields(&[b"a", b""]));
}

#[test]
fn fnv_finish_does_not_consume_state() {
    let mut h = Fnv::default();
    h.field(b"x");
    let first = h.finish();
    assert_eq!(h.finish(), first);
    h.field(b"y");
    assert_ne!(h.finish(), first);
}

proptest! {
    #[test]
    fn fnv_is_deterministic_and_fixed_width(a in prop::collection::vec(any::<u8>(), 0..64)) {
        let one = hash_fields(&[&a]);
        let two = hash_fields(&[&a]);
        prop_assert_eq!(&one, &two);
        prop_assert_eq!(one.len(), 16);
        prop_assert!(one.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn fnv_field_boundaries_matter(
        a in prop::collection::vec(any::<u8>(), 0..16),
        b in prop::collection::vec(any::<u8>(), 0..16),
        split in 0usize..32,
    ) {
        // Re-splitting the same bytes across two fields changes the hash,
        // unless the split lands on the original boundary.
        let joined: Vec<u8> = a.iter().chain(&b).copied().collect();
        let at = split.min(joined.len());
        prop_assume!(at != a.len());
        let (l, r) = joined.split_at(at);
        prop_assert_ne!(hash_fields(&[&a, &b]), hash_fields(&[l, r]));
    }

    #[test]
    fn fnv_write_is_incremental(
        a in prop::collection::vec(any::<u8>(), 0..32),
        b in prop::collection::vec(any::<u8>(), 0..32),
    ) {
        let mut whole = Fnv::default();
        let joined: Vec<u8> = a.iter().chain(&b).copied().collect();
        whole.write(&joined);
        let mut parts = Fnv::default();
        parts.write(&a);
        parts.write(&b);
        prop_assert_eq!(whole.finish(), parts.finish());
    }
}
