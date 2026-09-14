//! Consumption ledger, entitlement math, and the offline clock guard.
//!
//! Everything a license grants is measured here: tokens consumed, seconds
//! spent, and the high-water mark that makes a rolled-back system clock
//! detectable. The ledger is a tiny JSON file under `<app_data>/state/`, so a
//! paying user keeps their remaining allowance across launches without a
//! server (and without the license blob ever touching disk — that lives in the
//! OS keyring).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::provider::license::{LicensePayload, MeterMode};
use crate::util::fs::atomic_write;

/// Backwards clock drift we tolerate before calling it tampering. A timezone
/// change or NTP correction is far smaller than this; a deliberate rollback to
/// resurrect an expired license is far larger.
pub const CLOCK_SKEW_SECONDS: u64 = 300;

/// A message costs at least this much on the time meter (mirrors the web app,
/// so the two clients bill the same way).
pub const MIN_TIME_CHARGE_SECONDS: u64 = 60;

/// Persisted consumption, plus the clock high-water mark.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub seconds_used: u64,
    /// Greatest `now` we have ever observed. Never moves backwards.
    #[serde(default)]
    pub last_seen_unix: u64,
}

/// `<app_data>/state/` — where the provider config and ledger live.
pub fn state_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("state")
}

/// `<app_data>/state/provider.json` — provider settings, **never** the key.
pub fn config_path(app_data_dir: &Path) -> PathBuf {
    state_dir(app_data_dir).join("provider.json")
}

/// `<app_data>/state/provider-usage.json` — the ledger.
pub fn usage_path(app_data_dir: &Path) -> PathBuf {
    state_dir(app_data_dir).join("provider-usage.json")
}

/// Read/write handle for the ledger.
pub struct UsageStore {
    path: PathBuf,
}

impl UsageStore {
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            path: usage_path(app_data_dir),
        }
    }

    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Tolerant read: a missing or corrupt ledger means "nothing consumed
    /// yet", which is the only recovery that can't over-charge a user.
    pub async fn load(&self) -> Usage {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Usage::default(),
        }
    }

    pub async fn save(&self, usage: &Usage) -> Result<(), AppError> {
        let dir = self.path.parent().ok_or_else(|| AppError::Io {
            message: "provider usage path has no parent".into(),
        })?;
        tokio::fs::create_dir_all(dir).await.map_err(|e| AppError::Io {
            message: format!("create {}: {}", dir.display(), e),
        })?;
        let bytes = serde_json::to_vec_pretty(usage)?;
        atomic_write(&self.path, &bytes).await
    }
}

/// False when the clock has been moved backwards past the high-water mark
/// (minus a small skew allowance) — the offline-license anti-rollback check.
pub fn clock_ok(usage: &Usage, now: u64) -> bool {
    usage.last_seen_unix == 0 || now.saturating_add(CLOCK_SKEW_SECONDS) >= usage.last_seen_unix
}

/// Advance the high-water mark. Never lowers it, so the guard can't be reset
/// by waiting, and `touch_clock` is safe to call on every preflight.
pub fn touch_clock(usage: &mut Usage, now: u64) {
    usage.last_seen_unix = usage.last_seen_unix.max(now);
}

/// Remaining allowance, as the Settings panel shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entitlement {
    pub plan: String,
    pub meter: MeterMode,
    pub tokens_remaining: u64,
    pub seconds_remaining: u64,
    pub tokens_used: u64,
    pub seconds_used: u64,
    pub expires_at: u64,
}

pub fn entitlement(payload: &LicensePayload, usage: &Usage) -> Entitlement {
    Entitlement {
        plan: payload.plan.clone(),
        meter: payload.meter,
        tokens_remaining: payload.tokens.saturating_sub(usage.tokens_used),
        seconds_remaining: payload.seconds().saturating_sub(usage.seconds_used),
        tokens_used: usage.tokens_used,
        seconds_used: usage.seconds_used,
        expires_at: payload.expires_at,
    }
}

/// Refuse an operation the license can't pay for, *before* the provider call.
///
/// Takes both needs because a `Both` license (and a `Both` profile) can be
/// billed on either meter; each is checked against its own reserve.
pub fn precheck(
    payload: &LicensePayload,
    usage: &Usage,
    meter: MeterMode,
    need_tokens: u64,
    need_seconds: u64,
) -> Result<(), AppError> {
    if meter.allows_tokens() {
        if !payload.meter.allows_tokens() {
            return Err(AppError::NotEntitled {
                meter: MeterMode::Tokens.as_str().into(),
                remaining: 0,
            });
        }
        let remaining = payload.tokens.saturating_sub(usage.tokens_used);
        if remaining < need_tokens {
            return Err(AppError::NotEntitled {
                meter: MeterMode::Tokens.as_str().into(),
                remaining,
            });
        }
    }
    if meter.allows_time() {
        if !payload.meter.allows_time() {
            return Err(AppError::NotEntitled {
                meter: MeterMode::Time.as_str().into(),
                remaining: 0,
            });
        }
        let remaining = payload.seconds().saturating_sub(usage.seconds_used);
        if remaining < need_seconds {
            return Err(AppError::NotEntitled {
                meter: MeterMode::Time.as_str().into(),
                remaining,
            });
        }
    }
    Ok(())
}

/// Debit a completed call. Saturating everywhere: the ledger may only ever
/// reduce what's left, never wrap.
pub fn charge(usage: &mut Usage, meter: MeterMode, tokens: u64, seconds: u64) {
    if meter.allows_tokens() {
        usage.tokens_used = usage.tokens_used.saturating_add(tokens);
    }
    if meter.allows_time() {
        usage.seconds_used = usage.seconds_used.saturating_add(seconds);
    }
}

/// Character-based token estimate — the fallback for providers that don't
/// report usage. Same divisor as the web client so both agree.
pub fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    chars.div_ceil(2).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::license::LicensePayload;

    fn payload(meter: MeterMode, tokens: u64, minutes: u64) -> LicensePayload {
        LicensePayload {
            v: 1,
            license_id: "lic-1".into(),
            plan: "pro".into(),
            meter,
            tokens,
            minutes,
            agents: vec!["*".into()],
            expires_at: 0,
        }
    }

    #[test]
    fn clock_guard_rejects_a_rolled_back_clock() {
        let mut usage = Usage::default();
        assert!(clock_ok(&usage, 1_000), "a fresh ledger can't be tampered");
        touch_clock(&mut usage, 1_000);
        assert!(clock_ok(&usage, 1_000));
        assert!(clock_ok(&usage, 1_000 - CLOCK_SKEW_SECONDS), "within skew");
        assert!(!clock_ok(&usage, 1_000 - CLOCK_SKEW_SECONDS - 1), "past skew");
        // Never moves backwards.
        touch_clock(&mut usage, 500);
        assert_eq!(usage.last_seen_unix, 1_000);
        touch_clock(&mut usage, 5_000);
        assert_eq!(usage.last_seen_unix, 5_000);
    }

    #[test]
    fn entitlement_subtracts_usage_and_never_underflows() {
        let usage = Usage {
            tokens_used: 60_000,
            seconds_used: 120,
            last_seen_unix: 0,
        };
        let ent = entitlement(&payload(MeterMode::Both, 50_000, 10), &usage);
        assert_eq!(ent.tokens_remaining, 0, "over-consumption clamps at zero");
        assert_eq!(ent.seconds_remaining, 10 * 60 - 120);
        assert_eq!(ent.tokens_used, 60_000);
        assert_eq!(ent.meter, MeterMode::Both);
    }

    #[test]
    fn precheck_enforces_both_the_meter_and_the_allowance() {
        let usage = Usage::default();
        let token_plan = payload(MeterMode::Tokens, 1_000, 0);
        assert!(precheck(&token_plan, &usage, MeterMode::Tokens, 999, 0).is_ok());
        match precheck(&token_plan, &usage, MeterMode::Tokens, 1_001, 0) {
            Err(AppError::NotEntitled { meter, remaining }) => {
                assert_eq!(meter, "tokens");
                assert_eq!(remaining, 1_000);
            }
            other => panic!("expected NotEntitled, got {other:?}"),
        }
        // A token-only license cannot pay for the time meter at all.
        match precheck(&token_plan, &usage, MeterMode::Time, 0, 60) {
            Err(AppError::NotEntitled { meter, .. }) => assert_eq!(meter, "time"),
            other => panic!("expected NotEntitled, got {other:?}"),
        }
    }

    #[test]
    fn precheck_checks_both_meters_for_a_both_license() {
        let usage = Usage::default();
        let both = payload(MeterMode::Both, 10, 1);
        assert!(precheck(&both, &usage, MeterMode::Both, 10, 60).is_ok());
        match precheck(&both, &usage, MeterMode::Both, 11, 60) {
            Err(AppError::NotEntitled { meter, .. }) => assert_eq!(meter, "tokens"),
            other => panic!("expected NotEntitled, got {other:?}"),
        }
        match precheck(&both, &usage, MeterMode::Both, 10, 61) {
            Err(AppError::NotEntitled { meter, .. }) => assert_eq!(meter, "time"),
            other => panic!("expected NotEntitled, got {other:?}"),
        }
    }

    #[test]
    fn charge_only_debits_the_meters_the_license_unlocks() {
        let mut usage = Usage::default();
        charge(&mut usage, MeterMode::Tokens, 120, 45);
        assert_eq!(usage.tokens_used, 120);
        assert_eq!(usage.seconds_used, 0, "time is not billed on a token plan");

        charge(&mut usage, MeterMode::Both, 10, 30);
        assert_eq!(usage.tokens_used, 130);
        assert_eq!(usage.seconds_used, 30);
    }

    #[test]
    fn estimate_tokens_is_character_based_and_never_zero() {
        assert_eq!(estimate_tokens(""), 1);
        assert_eq!(estimate_tokens("hello"), 3);
        assert_eq!(estimate_tokens("سلام"), 2);
    }

    #[tokio::test]
    async fn usage_store_round_trips_and_survives_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = UsageStore::at(dir.path().join("state").join("provider-usage.json"));
        assert_eq!(store.load().await, Usage::default(), "missing file = zeroed");

        let usage = Usage {
            tokens_used: 42,
            seconds_used: 7,
            last_seen_unix: 1_700_000_000,
        };
        store.save(&usage).await.expect("save");
        assert_eq!(store.load().await, usage);
    }

    #[tokio::test]
    async fn usage_store_treats_corrupt_json_as_zeroed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state").join("provider-usage.json");
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .expect("mkdir");
        tokio::fs::write(&path, b"{not json").await.expect("write");
        let store = UsageStore::at(path);
        assert_eq!(store.load().await, Usage::default());
    }
}
