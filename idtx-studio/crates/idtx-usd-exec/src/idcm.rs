//! Rust-side default **idcm** (dynamic-variable) environment provider for the
//! `OpenExec` `Compute_Environment` node.
//!
//! Registered under the `"idcm"` prefix in the exec plugin's
//! `idtx::EnvironmentProviderRegistry`, so a stage authoring
//! `inputs:key = "idcm:CURRENT_TIME"` resolves to a value computed **fresh on
//! every `Resolve()`** — unlike the static [`crate::auth`] bucket, these reflect
//! the wall-clock instant the computation runs. Ported from the sibling
//! `idtx-flow-idcm` Godot extension's `IdcmVarProvider`
//! (`source/exec/IdcmEnvironmentProvider.h`), which registers the same prefix:
//!
//! ```cpp
//! idtx::EnvironmentProviderRegistry::Instance().RegisterProvider("idcm", &g_var_provider);
//! ```
//!
//! | key (de-prefixed)    | resolves to                                        |
//! |----------------------|----------------------------------------------------|
//! | `CURRENT_TIME`       | ISO-8601 UTC now, e.g. `2026-03-08T14:29:04Z`      |
//! | `CURRENT_TIME_LESS5` | ISO-8601 UTC now minus one hour                    |
//! | `INTERVAL`           | the literal `24h`                                  |
//! | (anything else)      | unresolved ([`None`])                              |
//!
//! These feed REST datasource query windows (a `from`/`to` timestamp pair and a
//! poll interval) authored as `${idcm:…}` tokens.
//!
//! The registration mechanics live in [`crate::env`]; this module supplies only
//! the `idcm:` [`EnvProvider`] and its default registration entry points.

use crate::env::{self, EnvProvider};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The routing prefix this provider is registered under; an authored
/// `inputs:key = "idcm:CURRENT_TIME"` is routed here with key `CURRENT_TIME`.
pub const IDCM_PREFIX: &str = "idcm";

/// Key resolving to the current instant as an ISO-8601 UTC string.
pub const KEY_CURRENT_TIME: &str = "CURRENT_TIME";
/// Key resolving to the current instant minus a fixed look-back window.
pub const KEY_CURRENT_TIME_LESS5: &str = "CURRENT_TIME_LESS5";
/// Key resolving to the fixed poll-interval literal.
pub const KEY_INTERVAL: &str = "INTERVAL";

/// Look-back applied by [`KEY_CURRENT_TIME_LESS5`].
///
/// The legacy key name is retained; the configured window is one hour (updated
/// together with INTERVAL in `83df7db`).
const LESS5_LOOKBACK: Duration = Duration::from_hours(1);

/// The configured fixed value of [`KEY_INTERVAL`].
const INTERVAL_VALUE: &str = "24h";

/// Resolves a de-prefixed `idcm:` key against dynamic built-ins.
struct IdcmVarProvider;

impl EnvProvider for IdcmVarProvider {
    fn resolve(&self, key: &str) -> Option<String> {
        resolve_var(key, SystemTime::now())
    }
}

/// Pure resolver, parameterized on `now` so it is deterministically testable.
fn resolve_var(key: &str, now: SystemTime) -> Option<String> {
    match key {
        KEY_CURRENT_TIME => Some(format_utc_iso8601(now)),
        KEY_CURRENT_TIME_LESS5 => Some(format_utc_iso8601(now - LESS5_LOOKBACK)),
        KEY_INTERVAL => Some(INTERVAL_VALUE.to_owned()),
        _ => None,
    }
}

/// Format a [`SystemTime`] as an ISO-8601 UTC string, e.g. `2026-03-08T14:29:04Z`
/// — the exact shape the sibling's `strftime("%Y-%m-%dT%H:%M:%SZ")` emits.
///
/// Inlined (no `chrono`/`time` dependency, matching `idtx-auth`'s inlined
/// base64): the civil-date split is Howard Hinnant's `civil_from_days`. Times
/// before the Unix epoch (which cannot arise from `SystemTime::now()`) clamp to
/// the epoch rather than panic.
fn format_utc_iso8601(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let (hour, min, sec) = (tod / 3_600, (tod % 3_600) / 60, tod % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Convert a count of days since the Unix epoch (1970-01-01) to a civil
/// `(year, month, day)`. Howard Hinnant's proleptic-Gregorian algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Register the default idcm provider into the resident plugin at `dir`.
///
/// Idempotent and best-effort — see [`env::register_provider_at`]. Wired into
/// [`crate::install`] alongside [`crate::auth`], so every host gets it for free.
pub fn register_default_idcm_provider_at(dir: &Path) {
    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    if env::register_provider_at(dir, IDCM_PREFIX, IdcmVarProvider) {
        #[cfg(feature = "metrics")]
        tracing::info!(
            ms = start.elapsed().as_secs_f64() * 1e3,
            "registered default OpenExec idcm provider"
        );
    }
}

/// Register the default idcm provider, resolving the plugin from the directory
/// [`crate::install`] uses ([`crate::plugin_dir`]).
pub fn register_default_idcm_provider() {
    register_default_idcm_provider_at(&crate::plugin_dir());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed instant formats to the exact ISO-8601 UTC string the sibling's
    /// `strftime` would produce. `1_709_908_144` s = 2024-03-08T14:29:04Z.
    #[test]
    fn current_time_is_iso8601_utc() {
        let t = UNIX_EPOCH + Duration::from_secs(1_709_908_144);
        assert_eq!(resolve_var(KEY_CURRENT_TIME, t).as_deref(), Some("2024-03-08T14:29:04Z"));
    }

    /// The legacy key subtracts the configured one-hour look-back.
    #[test]
    fn current_time_less5_subtracts_the_lookback() {
        let t = UNIX_EPOCH + Duration::from_secs(1_709_908_144);
        assert_eq!(
            resolve_var(KEY_CURRENT_TIME_LESS5, t).as_deref(),
            Some("2024-03-08T13:29:04Z"),
        );
    }

    /// `INTERVAL` is the fixed literal; unknown keys are unresolved.
    #[test]
    fn interval_literal_and_unknown_key() {
        let t = UNIX_EPOCH + Duration::from_secs(1_709_908_144);
        assert_eq!(resolve_var(KEY_INTERVAL, t).as_deref(), Some("24h"));
        assert_eq!(resolve_var("NOPE", t), None);
        assert_eq!(resolve_var("", t), None);
    }

    /// Epoch and a leap-day boundary exercise `civil_from_days`.
    #[test]
    fn date_boundaries() {
        assert_eq!(resolve_var(KEY_CURRENT_TIME, UNIX_EPOCH).as_deref(), Some("1970-01-01T00:00:00Z"));
        // 2024-02-29 (leap day) 23:59:59 UTC = 1_709_251_199 s.
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_251_199);
        assert_eq!(resolve_var(KEY_CURRENT_TIME, leap).as_deref(), Some("2024-02-29T23:59:59Z"));
    }
}
