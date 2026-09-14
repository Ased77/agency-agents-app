//! Custom AI provider — the invoke surface (contracts.md §C).
//!
//! Three gates run in order on every paid call, and the order matters:
//!
//! 1. **Offline mode** — [`AppState::require_network`] first, like every other
//!    outbound command.
//! 2. **Agents only** — a chat must name an agent, carry that agent's persona,
//!    and the provider profile must list the agent. There is no request shape
//!    without an agent.
//! 3. **Paid access** — a signature-verified license must cover the agent and
//!    still have allowance left on the profile's meter, before a single byte
//!    goes to the provider.
//!
//! Secrets never touch disk: the API key and the license blob live in the OS
//! keyring, while `state/provider.json` (settings) and
//! `state/provider-usage.json` (ledger) stay secret-free.

use chrono::Utc;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use crate::error::AppError;
use crate::github::auth::{KeychainSlot, SystemKeychain};
use crate::provider::license::{self, LicenseError, LicensePayload, LicenseStatus};
use crate::provider::license::KEYCHAIN_ACCOUNT_LICENSE;
use crate::provider::meter::{self, Entitlement, UsageStore};
use crate::provider::openai::{
    self, ChatEvent, ChatRequest, Charge, ProviderConfig, TestOutcome,
    KEYCHAIN_ACCOUNT_PROVIDER_KEY,
};
use crate::state::AppState;
use crate::util::fs::atomic_write;

/// Offline-mode feature label surfaced in the blocked-feature toast.
const FEATURE_PROVIDER: &str = "provider_chat";

fn now_unix() -> u64 {
    Utc::now().timestamp().max(0) as u64
}

fn keychain() -> SystemKeychain {
    SystemKeychain
}

/// Read the provider settings. A missing file means "not configured yet";
/// a corrupt one fails loudly rather than silently resetting a user's setup.
async fn read_config(state: &AppState) -> Result<ProviderConfig, AppError> {
    let path = meter::config_path(&state.app_data_dir);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| AppError::JsonParse {
            command: "provider_config_get".into(),
            message: e.to_string(),
            raw_excerpt: String::from_utf8_lossy(&bytes).chars().take(160).collect(),
        }),
        Err(_) => Ok(ProviderConfig::default()),
    }
}

async fn write_config(state: &AppState, config: &ProviderConfig) -> Result<(), AppError> {
    let dir = meter::state_dir(&state.app_data_dir);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| AppError::Io {
        message: format!("create {}: {}", dir.display(), e),
    })?;
    let bytes = serde_json::to_vec_pretty(config)?;
    atomic_write(&meter::config_path(&state.app_data_dir), &bytes).await
}

/// Verify the stored license against the pinned public key, applying the
/// clock guard first so a rolled-back clock can't resurrect anything.
async fn verified_license(
    state: &AppState,
) -> Result<(LicenseStatus, Option<LicensePayload>), AppError> {
    let pubkey = crate::license_pubkey();
    if !license::pubkey_configured(pubkey) {
        return Ok((LicenseStatus::unconfigured(), None));
    }
    let blob = match keychain().read(KEYCHAIN_ACCOUNT_LICENSE)? {
        Some(blob) => blob,
        None => return Ok((LicenseStatus::missing(), None)),
    };
    let usage = UsageStore::new(&state.app_data_dir).load().await;
    let now = now_unix();
    if !meter::clock_ok(&usage, now) {
        return Ok((LicenseStatus::from_error(LicenseError::Clock), None));
    }
    match license::verify_blob(&blob, pubkey, now) {
        Ok(payload) => Ok((LicenseStatus::ok(&payload), Some(payload))),
        Err(err) => Ok((LicenseStatus::from_error(err), None)),
    }
}

// --------------------------------------------------------------- settings

#[tauri::command]
pub async fn provider_config_get(state: State<'_, AppState>) -> Result<ProviderConfig, AppError> {
    read_config(&state).await
}

/// Persist provider settings. The base URL is validated against the SSRF gate
/// here, so a blocked host is rejected at *save* time (loudly, in the UI)
/// instead of at the first chat.
#[tauri::command]
pub async fn provider_config_set(
    state: State<'_, AppState>,
    config: ProviderConfig,
) -> Result<ProviderConfig, AppError> {
    let mut next = config;
    next.label = next.label.trim().to_string();
    next.model = next.model.trim().to_string();
    next.base_url = next.base_url.trim().to_string();
    next.consented_hosts = next
        .consented_hosts
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    if !next.base_url.is_empty() {
        next.base_url = openai::validate_base_url(
            &next.base_url,
            next.allow_private_host,
            &next.consented_hosts,
        )?;
    }
    if next.agents.is_empty() {
        next.agents = vec!["*".to_string()];
    }
    write_config(&state, &next).await?;
    Ok(next)
}

#[tauri::command]
pub async fn provider_key_set(key: String) -> Result<(), AppError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidArgument {
            message: "the api key is empty".into(),
        });
    }
    keychain().write(KEYCHAIN_ACCOUNT_PROVIDER_KEY, trimmed)
}

/// Whether an API key is stored. Never returns the key itself.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderKeyStatus {
    pub configured: bool,
}

#[tauri::command]
pub async fn provider_key_status() -> Result<ProviderKeyStatus, AppError> {
    Ok(ProviderKeyStatus {
        configured: keychain().read(KEYCHAIN_ACCOUNT_PROVIDER_KEY)?.is_some(),
    })
}

#[tauri::command]
pub async fn provider_key_clear() -> Result<(), AppError> {
    keychain().delete(KEYCHAIN_ACCOUNT_PROVIDER_KEY)
}

// ---------------------------------------------------------------- license

#[tauri::command]
pub async fn license_status(state: State<'_, AppState>) -> Result<LicenseStatus, AppError> {
    Ok(verified_license(&state).await?.0)
}

/// Verify + store a pasted license. Invalid licenses are rejected before they
/// are written, so a bad paste never leaves the app in a half-licensed state.
#[tauri::command]
pub async fn license_set(
    state: State<'_, AppState>,
    blob: String,
) -> Result<LicenseStatus, AppError> {
    let pubkey = crate::license_pubkey();
    if !license::pubkey_configured(pubkey) {
        return Err(LicenseError::Unconfigured.into());
    }
    let usage = UsageStore::new(&state.app_data_dir).load().await;
    let now = now_unix();
    if !meter::clock_ok(&usage, now) {
        return Err(LicenseError::Clock.into());
    }
    let payload = license::verify_blob(&blob, pubkey, now)?;
    keychain().write(KEYCHAIN_ACCOUNT_LICENSE, blob.trim())?;
    Ok(LicenseStatus::ok(&payload))
}

#[tauri::command]
pub async fn license_clear() -> Result<LicenseStatus, AppError> {
    keychain().delete(KEYCHAIN_ACCOUNT_LICENSE)?;
    Ok(LicenseStatus::missing())
}

/// Remaining allowance for the Settings panel, or `None` when unlicensed.
#[tauri::command]
pub async fn entitlement_get(
    state: State<'_, AppState>,
) -> Result<Option<Entitlement>, AppError> {
    let (_, payload) = verified_license(&state).await?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let usage = UsageStore::new(&state.app_data_dir).load().await;
    Ok(Some(meter::entitlement(&payload, &usage)))
}

// --------------------------------------------------------------- chat/test

/// Configure-time connectivity probe. Runs the real client (same URL gate,
/// same body builder, probe persona) but skips the license/meter gate —
/// there is nothing to spend before setup is finished.
#[tauri::command]
pub async fn provider_test(state: State<'_, AppState>) -> Result<TestOutcome, AppError> {
    state.require_network("provider_test").await?;
    let config = read_config(&state).await?;
    let key = keychain().read(KEYCHAIN_ACCOUNT_PROVIDER_KEY)?;
    // The key is required, but the license is deliberately *not* — nothing has
    // been spent yet, and setup must work before the first license is pasted.
    if config.base_url.trim().is_empty() || config.model.trim().is_empty() {
        return Err(AppError::ProviderNotConfigured {
            message: "a base URL and model are required".into(),
        });
    }
    let key = key.ok_or(AppError::ProviderKeyMissing)?;
    openai::test_connection(&config, &key).await
}

#[tauri::command]
pub async fn provider_chat_stream(
    state: State<'_, AppState>,
    request: ChatRequest,
    on_event: Channel<ChatEvent>,
) -> Result<Charge, AppError> {
    // --- agents only ---------------------------------------------------
    if request.agent_slug.trim().is_empty() {
        return Err(AppError::InvalidArgument {
            message: "an agent slug is required".into(),
        });
    }
    if request.persona.trim().is_empty() {
        return Err(AppError::InvalidArgument {
            message: "an agent persona is required".into(),
        });
    }
    if request.message.trim().is_empty() {
        return Err(AppError::InvalidArgument {
            message: "the message is empty".into(),
        });
    }

    state.require_network(FEATURE_PROVIDER).await?;
    let config = read_config(&state).await?;
    let key = keychain().read(KEYCHAIN_ACCOUNT_PROVIDER_KEY)?;
    if !config.ready(key.is_some()) {
        return Err(AppError::ProviderNotConfigured {
            message: "the provider is disabled or incomplete".into(),
        });
    }
    let key = key.ok_or(AppError::ProviderKeyMissing)?;
    if !config.covers_agent(&request.agent_slug) {
        return Err(AppError::NotEntitled {
            meter: "agent".into(),
            remaining: 0,
        });
    }

    // --- paid access ---------------------------------------------------
    let usage_store = UsageStore::new(&state.app_data_dir);
    let mut usage = usage_store.load().await;
    let now = now_unix();
    if !meter::clock_ok(&usage, now) {
        return Err(LicenseError::Clock.into());
    }
    meter::touch_clock(&mut usage, now);

    let blob = keychain()
        .read(KEYCHAIN_ACCOUNT_LICENSE)?
        .ok_or(LicenseError::Missing)?;
    let payload = license::verify_blob(&blob, crate::license_pubkey(), now)?;
    if !payload.covers_agent(&request.agent_slug) {
        return Err(LicenseError::Agent.into());
    }

    let system = openai::system_prompt(
        if request.agent_name.trim().is_empty() {
            &request.agent_slug
        } else {
            &request.agent_name
        },
        &request.persona,
        &request.lang,
    );

    // Reserve before spending: a token estimate across prompt + answer, or the
    // one-minute minimum on the time meter.
    let need_tokens = meter::estimate_tokens(&system)
        .saturating_add(meter::estimate_tokens(&request.message));
    let need_seconds = if config.meter.allows_time() {
        meter::MIN_TIME_CHARGE_SECONDS
    } else {
        0
    };
    meter::precheck(
        &payload,
        &usage,
        config.meter,
        if config.meter.allows_tokens() { need_tokens } else { 0 },
        need_seconds,
    )?;

    // Cap the response at what's actually left, so a runaway answer can't
    // overspend the allowance the pre-check just approved.
    let ent = meter::entitlement(&payload, &usage);
    let budget = openai::Budget {
        tokens: ent.tokens_remaining,
        seconds: ent.seconds_remaining,
    };

    let charge = openai::stream_chat(
        &config,
        &key,
        &system,
        &request.history,
        &request.message,
        Some(budget),
        |event| {
            // A closed channel means the UI moved on; the call still
            // completes so the ledger stays truthful.
            let _ = on_event.send(event);
        },
    )
    .await?;

    meter::charge(&mut usage, config.meter, charge.tokens, charge.seconds);
    usage_store.save(&usage).await?;
    Ok(charge)
}
