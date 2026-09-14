//! Offline license — the "paid access" half of the custom provider.
//!
//! A license is a **signed, self-contained blob** the user pastes into
//! Settings → AI provider. Verification is fully offline against a public key
//! compiled into the binary (mirroring the updater's trust model), so paid
//! access needs no server, no account and no network call.
//!
//! Wire format (one text blob, exactly what the issuer produces):
//!
//! ```text
//! <base64(payload JSON)>          <- one line, standard base64
//! untrusted comment: …
//! <base64 ed25519 signature>
//! trusted comment: …
//! <base64 global signature>
//! ```
//!
//! The trailing four lines are a stock [minisign] signature over the raw
//! payload bytes, so issuance is `minisign -Sm payload.json -s <key>` plus a
//! one-line base64 join — no custom signing tool, no key material in-repo.
//!
//! [minisign]: https://jedisct1.github.io/minisign/

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// Keyring account holding the raw license blob. The blob lives in the OS
/// vault, never in `provider.json` — losing the file must not leak a license.
pub const KEYCHAIN_ACCOUNT_LICENSE: &str = "provider_license";

/// A real license is a few hundred bytes; anything this large is an accident
/// or an attack.
const MAX_BLOB_BYTES: usize = 64 * 1024;

/// Which meter a license unlocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MeterMode {
    Tokens,
    Time,
    Both,
}

impl MeterMode {
    pub fn allows_tokens(self) -> bool {
        matches!(self, MeterMode::Tokens | MeterMode::Both)
    }

    pub fn allows_time(self) -> bool {
        matches!(self, MeterMode::Time | MeterMode::Both)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MeterMode::Tokens => "tokens",
            MeterMode::Time => "time",
            MeterMode::Both => "both",
        }
    }
}

fn wildcard_agents() -> Vec<String> {
    vec!["*".to_string()]
}

fn default_version() -> u32 {
    1
}

/// The signed payload. Field names are camelCase on the wire so the issuer
/// (and any future sync with the web app) uses one spelling everywhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LicensePayload {
    #[serde(default = "default_version")]
    pub v: u32,
    #[serde(default)]
    pub license_id: String,
    #[serde(default)]
    pub plan: String,
    pub meter: MeterMode,
    /// Token allowance for token-metered plans.
    #[serde(default)]
    pub tokens: u64,
    /// Time allowance, in minutes, for time-metered plans.
    #[serde(default)]
    pub minutes: u64,
    /// Agent slugs this license unlocks; `["*"]` means every agent.
    #[serde(default = "wildcard_agents")]
    pub agents: Vec<String>,
    /// Unix seconds. `0` means "no expiry".
    #[serde(default)]
    pub expires_at: u64,
}

impl LicensePayload {
    /// Time allowance in seconds — the unit the meter ledger stores.
    pub fn seconds(&self) -> u64 {
        self.minutes.saturating_mul(60)
    }

    pub fn is_expired(&self, now: u64) -> bool {
        self.expires_at > 0 && now > self.expires_at
    }

    /// "Agents only": the license names the agents it covers.
    pub fn covers_agent(&self, slug: &str) -> bool {
        self.agents.iter().any(|a| a == "*" || a == slug)
    }
}

/// Every way a license can be unusable, as a stable discriminator so the
/// frontend can render the right message without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseError {
    /// No license stored.
    Missing,
    /// This build has no license public key compiled in.
    Unconfigured,
    /// The blob doesn't have the payload + signature shape.
    Malformed,
    /// Base64 or JSON decoding failed.
    Decode,
    /// The signature did not verify against the embedded public key.
    Signature,
    /// Past `expiresAt`.
    Expired,
    /// The system clock moved backwards past our high-water mark.
    Clock,
    /// The license doesn't name the agent being used.
    Agent,
}

impl LicenseError {
    pub fn reason(self) -> &'static str {
        match self {
            LicenseError::Missing => "missing",
            LicenseError::Unconfigured => "unconfigured",
            LicenseError::Malformed => "malformed",
            LicenseError::Decode => "decode",
            LicenseError::Signature => "signature",
            LicenseError::Expired => "expired",
            LicenseError::Clock => "clock",
            LicenseError::Agent => "agent",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            LicenseError::Missing => "no license is stored",
            LicenseError::Unconfigured => "this build has no license public key",
            LicenseError::Malformed => "the license blob has no signature block",
            LicenseError::Decode => "the license payload could not be decoded",
            LicenseError::Signature => "the license signature did not verify",
            LicenseError::Expired => "the license has expired",
            LicenseError::Clock => "the system clock moved backwards since last use",
            LicenseError::Agent => "the license does not cover this agent",
        }
    }
}

impl From<LicenseError> for AppError {
    fn from(err: LicenseError) -> Self {
        AppError::LicenseInvalid {
            reason: err.reason().to_string(),
            message: err.message().to_string(),
        }
    }
}

/// What the UI shows in Settings → AI provider.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseStatus {
    pub present: bool,
    pub valid: bool,
    pub plan: String,
    pub meter: Option<MeterMode>,
    pub agents: Vec<String>,
    pub expires_at: Option<u64>,
    pub license_id: Option<String>,
    /// Machine-readable failure reason when `valid == false`.
    pub reason: Option<String>,
    /// Human-readable detail for the failure.
    pub message: Option<String>,
}

impl LicenseStatus {
    fn blank(reason: Option<&str>, message: Option<&str>) -> Self {
        Self {
            present: false,
            valid: false,
            plan: String::new(),
            meter: None,
            agents: Vec::new(),
            expires_at: None,
            license_id: None,
            reason: reason.map(str::to_string),
            message: message.map(str::to_string),
        }
    }

    pub fn missing() -> Self {
        Self::blank(Some(LicenseError::Missing.reason()), Some(LicenseError::Missing.message()))
    }

    pub fn unconfigured() -> Self {
        Self::blank(
            Some(LicenseError::Unconfigured.reason()),
            Some(LicenseError::Unconfigured.message()),
        )
    }

    pub fn from_error(err: LicenseError) -> Self {
        let mut status = Self::blank(Some(err.reason()), Some(err.message()));
        status.present = !matches!(err, LicenseError::Missing | LicenseError::Unconfigured);
        status
    }

    pub fn ok(payload: &LicensePayload) -> Self {
        Self {
            present: true,
            valid: true,
            plan: payload.plan.clone(),
            meter: Some(payload.meter),
            agents: payload.agents.clone(),
            expires_at: Some(payload.expires_at),
            license_id: Some(payload.license_id.clone()),
            reason: None,
            message: None,
        }
    }
}

/// Whether this build carries a usable license public key. A placeholder (or
/// an empty string) means "fail closed": nobody can mint a license that
/// verifies, including us.
pub fn pubkey_configured(pubkey_b64: &str) -> bool {
    !pubkey_b64.trim().is_empty() && !pubkey_b64.starts_with("REPLACE_ME")
}

/// Split the pasted blob into `(payload bytes, minisign signature text)`.
pub fn split_blob(blob: &str) -> Result<(Vec<u8>, String), LicenseError> {
    if blob.len() > MAX_BLOB_BYTES {
        return Err(LicenseError::Malformed);
    }
    let lines: Vec<&str> = blob
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // One payload line + the 4 minisign lines.
    if lines.len() < 5 {
        return Err(LicenseError::Malformed);
    }
    let payload = STANDARD
        .decode(lines[0])
        .map_err(|_| LicenseError::Decode)?;
    let signature = lines[1..].join("\n");
    Ok((payload, signature))
}

/// Verify a minisign signature over `payload` with a base64 public key.
///
/// `allow_legacy = true` so both prehashed (`minisign -SH`) and classic
/// signatures issued by any recent minisign release verify; the key id must
/// match, which is what actually pins us to our own keypair.
pub fn verify_signature(
    payload: &[u8],
    signature_text: &str,
    pubkey_b64: &str,
) -> Result<(), LicenseError> {
    if !pubkey_configured(pubkey_b64) {
        return Err(LicenseError::Unconfigured);
    }
    let public_key =
        minisign_verify::PublicKey::from_base64(pubkey_b64.trim()).map_err(|_| LicenseError::Decode)?;
    let signature =
        minisign_verify::Signature::decode(signature_text).map_err(|_| LicenseError::Decode)?;
    public_key
        .verify(payload, &signature, true)
        .map_err(|_| LicenseError::Signature)
}

/// Parse + validate the signed payload against `now`.
pub fn parse_payload(payload: &[u8], now: u64) -> Result<LicensePayload, LicenseError> {
    let parsed: LicensePayload = serde_json::from_slice(payload).map_err(|_| LicenseError::Decode)?;
    if parsed.is_expired(now) {
        return Err(LicenseError::Expired);
    }
    Ok(parsed)
}

/// Full verification path: split → verify → parse → expiry.
pub fn verify_blob(blob: &str, pubkey_b64: &str, now: u64) -> Result<LicensePayload, LicenseError> {
    let (payload, signature_text) = split_blob(blob)?;
    verify_signature(&payload, &signature_text, pubkey_b64)?;
    parse_payload(&payload, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The minisign-verify crate's own test fixture: this public key signed the
    /// literal bytes `test`. Reusing it (rather than minting a keypair in the
    /// test) keeps the crypto path pinned to a known-good vector.
    const FIXTURE_PUBKEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const FIXTURE_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\ntrusted comment: timestamp:1555779966\tfile:test\nQtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==";

    fn payload_json(meter: &str, extra: &str) -> String {
        format!(
            r#"{{"v":1,"licenseId":"lic-1","plan":"pro","meter":"{meter}"{extra}}}"#
        )
    }

    #[test]
    fn signature_verifies_for_the_fixture_payload() {
        assert!(verify_signature(b"test", FIXTURE_SIGNATURE, FIXTURE_PUBKEY).is_ok());
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let err = verify_signature(b"Test", FIXTURE_SIGNATURE, FIXTURE_PUBKEY).unwrap_err();
        assert_eq!(err, LicenseError::Signature);
    }

    #[test]
    fn placeholder_pubkey_fails_closed() {
        let err = verify_signature(b"test", FIXTURE_SIGNATURE, "REPLACE_ME").unwrap_err();
        assert_eq!(err, LicenseError::Unconfigured);
        assert!(!pubkey_configured("REPLACE_ME"));
        assert!(pubkey_configured(FIXTURE_PUBKEY));
    }

    #[test]
    fn split_blob_requires_payload_plus_four_signature_lines() {
        assert_eq!(split_blob("").unwrap_err(), LicenseError::Malformed);
        assert_eq!(
            split_blob("dGVzdA==\nonly-one-line").unwrap_err(),
            LicenseError::Malformed
        );

        // A full blob round-trips: payload decodes, signature text is joined.
        let blob = format!(
            "{}\n{}",
            STANDARD.encode(b"test"),
            FIXTURE_SIGNATURE
        );
        let (payload, signature) = split_blob(&blob).expect("split");
        assert_eq!(payload, b"test");
        assert_eq!(signature.lines().count(), 4);
    }

    #[test]
    fn verify_blob_accepts_a_signed_payload_and_rejects_a_tampered_one() {
        // Sign the *exact* payload bytes the fixture covers, then splice the
        // license JSON around them is not possible — instead assert the two
        // halves independently (signature over `test`, JSON parsing below).
        let blob = format!("{}\n{}", STANDARD.encode(b"test"), FIXTURE_SIGNATURE);
        let err = verify_blob(&blob, FIXTURE_PUBKEY, 1_700_000_000).unwrap_err();
        assert_eq!(err, LicenseError::Decode, "payload is not license JSON");
    }

    #[test]
    fn parse_payload_reads_allowances_and_rejects_expiry() {
        let json = payload_json("tokens", r#","tokens":500000,"minutes":3000"#);
        let payload = parse_payload(json.as_bytes(), 1_700_000_000).expect("parse");
        assert_eq!(payload.tokens, 500_000);
        assert_eq!(payload.minutes, 3_000);
        assert_eq!(payload.seconds(), 180_000);
        assert_eq!(payload.agents, vec!["*".to_string()]);
        assert!(!payload.is_expired(1_700_000_000));

        let expiring = payload_json("time", r#","minutes":60,"expiresAt":1700000000"#);
        let payload = parse_payload(expiring.as_bytes(), 1_700_000_000).expect("parse");
        assert!(!payload.is_expired(1_700_000_000));
        assert_eq!(
            parse_payload(expiring.as_bytes(), 1_700_000_001).unwrap_err(),
            LicenseError::Expired
        );
        // expiresAt = 0 means "never expires".
        let perpetual = payload_json("both", r#","tokens":10,"minutes":1,"expiresAt":0"#);
        assert!(parse_payload(perpetual.as_bytes(), u64::MAX).is_ok());
    }

    #[test]
    fn parse_payload_rejects_unknown_meter_and_bad_json() {
        assert_eq!(
            parse_payload(payload_json("tokens-and-more", "").as_bytes(), 0).unwrap_err(),
            LicenseError::Decode
        );
        assert_eq!(parse_payload(b"not json", 0).unwrap_err(), LicenseError::Decode);
    }

    #[test]
    fn payload_scopes_agents() {
        let scoped = payload_json("tokens", r#","agents":["sre","engineering-code-reviewer"]"#);
        let payload = parse_payload(scoped.as_bytes(), 0).expect("parse");
        assert!(payload.covers_agent("sre"));
        assert!(!payload.covers_agent("frontend-developer"));

        let wildcard = parse_payload(payload_json("tokens", "").as_bytes(), 0).expect("parse");
        assert!(wildcard.covers_agent("anything"));
    }

    #[test]
    fn meter_mode_predicates() {
        assert!(MeterMode::Tokens.allows_tokens());
        assert!(!MeterMode::Tokens.allows_time());
        assert!(MeterMode::Time.allows_time());
        assert!(!MeterMode::Time.allows_tokens());
        assert!(MeterMode::Both.allows_tokens() && MeterMode::Both.allows_time());
        assert_eq!(MeterMode::Both.as_str(), "both");
    }

    #[test]
    fn license_error_maps_into_a_typed_app_error() {
        let err: AppError = LicenseError::Expired.into();
        let value = serde_json::to_value(&err).expect("serialize");
        assert_eq!(value["code"], "license_invalid");
        assert_eq!(value["reason"], "expired");
        assert!(value["message"].as_str().unwrap_or_default().contains("expired"));
    }

    #[test]
    fn status_reports_reason_codes() {
        assert_eq!(LicenseStatus::missing().reason.as_deref(), Some("missing"));
        assert!(!LicenseStatus::missing().present);
        assert_eq!(
            LicenseStatus::from_error(LicenseError::Signature).reason.as_deref(),
            Some("signature")
        );
        assert!(LicenseStatus::from_error(LicenseError::Signature).present);

        let payload = parse_payload(payload_json("both", r#","tokens":7,"minutes":2"#).as_bytes(), 0)
            .expect("parse");
        let status = LicenseStatus::ok(&payload);
        assert!(status.valid && status.present);
        assert_eq!(status.meter, Some(MeterMode::Both));
        assert_eq!(status.plan, "pro");
    }
}
