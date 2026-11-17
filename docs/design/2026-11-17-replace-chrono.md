# Replace chrono with time

Status: design, for review before code. Unblocks the release: the release security gate (osv-scanner) fails on RUSTSEC-2026-0543, "chrono is unmaintained", which has no patched version. Rule for unfixable advisories: a time-boxed ignore only when the upstream is maintained; otherwise replace the dependency. chrono is a direct dependency of nab only (`cargo tree -i chrono`), so nab can remove it.

## Choice

`time` 0.3, already in the lock file through `cookie`, `cookie_store` and `rust-mcp-sdk`, and one of the two replacements the advisory names. No new crate enters the tree. `jiff`, the other, would add one.

Direct dependency: `time = { version = "0.3", features = ["serde", "formatting", "parsing", "macros"] }`.

## Mapping

| chrono | time |
|---|---|
| `DateTime<Utc>`, `Utc::now()` | `OffsetDateTime`, `OffsetDateTime::now_utc()` |
| `chrono::Duration::seconds(n)` / `hours(n)` | `time::Duration::seconds(n)` / `hours(n)` |
| `a.signed_duration_since(b).num_seconds()` / `num_days()` | `(a - b).whole_seconds()` / `whole_days()` |
| `dt.timestamp()` | `dt.unix_timestamp()` |
| `timestamp_nanos_opt().unwrap_or(0)` | `unix_timestamp_nanos()` |
| `DateTime::from_timestamp(s, 0)` / `from_timestamp_secs(s)` | `OffsetDateTime::from_unix_timestamp(s).ok()` |
| `DateTime::parse_from_rfc3339(s)` | `OffsetDateTime::parse(s, &Rfc3339)`, then `to_offset(UtcOffset::UTC)` |
| `NaiveDate::from_ymd_opt(y, m, d).is_some()` | `Date::from_calendar_date(y, Month::try_from(m as u8)?, d as u8).is_ok()`, with `m` and `d` range-checked before the cast |
| `format("%Y-%m-%dT%H:%M:%SZ")` | a `format_description!` constant, `[year]-[month]-[day]T[hour]:[minute]:[second]Z` |
| `format("%Y%m%d-%H%M%S")` | `[year][month][day]-[hour][minute][second]` |

## Saved data stays readable

Timestamps written to disk by chrono's serde (RFC 3339, `Z`, 0, 3, 6 or 9 fraction digits): watch storage (`Watch.created_at`, `last_check_at`, `last_change_at`; `WatchSnapshot.captured_at`), the fingerprint version cache (`BrowserVersions.last_updated`, `safari_last_checked`), and `LookupResult.fetched_at`, which is serialized into tool output rather than stored. These fields take `#[serde(with = "time::serde::rfc3339")]` (`::option` for `Option`). Telemetry's stamp file is plain text written by `format_stamp` and read by `parse_stamp`; both keep the same text.

## Public API

`fetched_at`, the watch structs' timestamp fields and the fingerprint cache fields are public and change type, so the release that carries this is 0.13.0 (it also carries #414's new `ConversionResult` field).

## Tests

Characterization tests written before the swap, green on chrono and required green after:
- C1. Each persisted struct deserializes JSON holding chrono-written timestamps in all four fraction widths, and re-serializes to a string that parses back to the same instant.
- C2. `format_stamp` and `parse_stamp` round-trip; `parse_stamp` accepts a stamp written by the current code.
- C3. The telemetry date check accepts 2024-02-29 and rejects 2023-02-29, month 13 and day 0.
- C4. `timestamp_from_activity_urn` keeps its current output for the existing doc-test URN.

Then the swap; `cargo tree -i chrono` must report nothing, and the release gate's osv-scanner run must pass on the branch.
