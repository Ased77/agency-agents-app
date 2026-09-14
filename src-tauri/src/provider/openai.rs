//! OpenAI-compatible provider client (streaming) and the SSRF gate.
//!
//! One code path serves DeepSeek, Qwen, OpenRouter, Groq, vLLM, llama.cpp and
//! Ollama's OpenAI shim: `POST {base_url}/chat/completions` with `stream:true`.
//! Deliberately **not** Claude/Anthropic — the feature is explicitly the
//! bring-your-own (non-Claude) provider path, and keeping vendor quirks out of
//! this file is what makes the contract stable.
//!
//! Two guards live here:
//!
//! * **SSRF** — every call re-validates the base URL with
//!   [`util::net::is_public_host`][is_public_host]. Loopback / RFC1918 /
//!   link-local hosts are refused unless the user explicitly consented to
//!   *that exact host* (Ollama and friends are local by design).
//! * **Agents only** — [`system_prompt`] always frames an agent persona; there
//!   is no request shape without one, and the command layer only reaches this
//!   module through the license + meter preflight.
//!
//! [is_public_host]: crate::util::net::is_public_host

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::provider::license::MeterMode;
use crate::provider::meter::estimate_tokens;
use crate::util::net::is_public_host;

/// Keyring account for the provider API key. Never written to `provider.json`.
pub const KEYCHAIN_ACCOUNT_PROVIDER_KEY: &str = "provider_api_key";

/// Hard cap on a single response, so a hostile/broken endpoint can't stream us
/// out of memory.
const MAX_RESPONSE_CHARS: usize = 200_000;

/// Whole-request budget (connect + all chunks).
const REQUEST_TIMEOUT_SECONDS: u64 = 180;

/// How many prior turns ride along as context.
pub const HISTORY_LIMIT: usize = 10;

fn default_enabled() -> bool {
    true
}

fn default_meter() -> MeterMode {
    MeterMode::Tokens
}

fn default_rate_tokens() -> u64 {
    2000
}

fn default_rate_minute() -> u64 {
    30_000
}

fn wildcard_agents() -> Vec<String> {
    vec!["*".to_string()]
}

/// Provider settings, persisted at `<app_data>/state/provider.json`.
///
/// Deliberately secret-free: the API key lives in the OS keyring and the
/// license blob in the keyring too, so this file can be read, logged, backed
/// up and diffed without leaking anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// Which allowance a chat debits. The license must unlock the same meter.
    #[serde(default = "default_meter")]
    pub meter: MeterMode,
    /// Display/charging rate for the token meter (Toman per 1k tokens).
    #[serde(default = "default_rate_tokens")]
    pub toman_per_1k_tokens: u64,
    /// Display/charging rate for the time meter (Toman per minute).
    #[serde(default = "default_rate_minute")]
    pub toman_per_minute: u64,
    /// Agent slugs this credential may serve. `["*"]` = any licensed agent.
    #[serde(default = "wildcard_agents")]
    pub agents: Vec<String>,
    /// Master switch for the private-endpoint exemption. Off = the SSRF guard
    /// is absolute.
    #[serde(default)]
    pub allow_private_host: bool,
    /// Hosts the user explicitly approved. Only consulted when
    /// `allow_private_host` is on, and matched exactly.
    #[serde(default)]
    pub consented_hosts: Vec<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            label: String::new(),
            base_url: String::new(),
            model: String::new(),
            meter: MeterMode::Tokens,
            toman_per_1k_tokens: default_rate_tokens(),
            toman_per_minute: default_rate_minute(),
            agents: wildcard_agents(),
            allow_private_host: false,
            consented_hosts: Vec::new(),
        }
    }
}

impl ProviderConfig {
    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    pub fn ready(&self, has_key: bool) -> bool {
        self.enabled
            && !self.base_url.trim().is_empty()
            && !self.model.trim().is_empty()
            && has_key
            && !self.agents.is_empty()
    }

    /// "Agents only": the profile names the agents this credential serves.
    pub fn covers_agent(&self, slug: &str) -> bool {
        self.agents.iter().any(|a| a == "*" || a == slug)
    }
}

/// Normalize + validate a base URL.
///
/// Public-routable hosts always pass. Loopback / RFC1918 / link-local / CGNAT
/// hosts need both `allow_private_host` **and** the exact host in
/// `consented_hosts`, so a malicious catalog can't point the app at
/// `169.254.169.254` just because the user once allowed `localhost`.
pub fn validate_base_url(
    raw: &str,
    allow_private: bool,
    consented: &[String],
) -> Result<String, AppError> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(AppError::ProviderNotConfigured {
            message: "base URL is empty".into(),
        });
    }
    if trimmed.len() > 2048 {
        return Err(AppError::InvalidArgument {
            message: "base URL is too long".into(),
        });
    }
    let url = url::Url::parse(&trimmed).map_err(|e| AppError::InvalidArgument {
        message: format!("invalid base URL: {e}"),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::InvalidArgument {
            message: "base URL must use http or https".into(),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::InvalidArgument {
            message: "base URL must not embed credentials".into(),
        });
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::InvalidArgument {
            message: "base URL has no host".into(),
        })?
        .to_string();
    if !is_public_host(&host) {
        let approved = allow_private
            && consented
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(&host));
        if !approved {
            return Err(AppError::ProviderBlockedHost { host });
        }
    }
    Ok(trimmed)
}

/// Re-validate the stored config before any outbound call (defense in depth:
/// the file could have been edited by hand).
pub fn resolve_endpoint(config: &ProviderConfig) -> Result<String, AppError> {
    validate_base_url(
        &config.base_url,
        config.allow_private_host,
        &config.consented_hosts,
    )?;
    Ok(config.endpoint())
}

// ------------------------------------------------------------------ prompt

/// Frame an agent persona as the system message. "Agents only" in one line:
/// no caller can build a request without going through here.
pub fn system_prompt(agent_name: &str, persona: &str, lang: &str) -> String {
    let language = if lang == "fa" {
        "Always answer in fluent Persian (فارسی), even when the user writes in English."
    } else {
        "Always answer in the user's language."
    };
    [
        format!("You are \"{agent_name}\", a specialised agent in the Agency Agents catalog."),
        persona.trim().to_string(),
        language.to_string(),
        "Stay in character, be concrete, and never invent capabilities you don't have.".to_string(),
    ]
    .iter()
    .filter(|part| !part.trim().is_empty())
    .cloned()
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// One chat turn on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// One agent chat turn, as the webview asks for it.
///
/// `agent_slug` and `persona` are **required** — they are how "agents only" is
/// enforced at the boundary rather than by convention.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    /// Corpus slug of the agent — the unit the license scopes.
    pub agent_slug: String,
    /// Display name, used to frame the persona.
    #[serde(default)]
    pub agent_name: String,
    /// The agent's markdown persona (corpus body).
    pub persona: String,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    pub message: String,
    /// Locale hint for the system prompt: `"fa"` or `"en"`.
    #[serde(default)]
    pub lang: String,
}

/// Build the request body shared by the streaming and test paths.
pub fn build_body(
    model: &str,
    stream: bool,
    system: &str,
    history: &[ChatTurn],
    message: &str,
) -> serde_json::Value {
    let mut messages = vec![serde_json::json!({ "role": "system", "content": system })];
    let start = history.len().saturating_sub(HISTORY_LIMIT);
    for turn in &history[start..] {
        messages.push(serde_json::json!({ "role": turn.role, "content": turn.content }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": message }));

    let mut body = serde_json::json!({
        "model": model,
        "stream": stream,
        "messages": messages,
    });
    if stream {
        body["stream_options"] = serde_json::json!({ "include_usage": true });
    }
    body
}

// --------------------------------------------------------------------- SSE

/// What one server-sent-events line means to us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkPiece {
    Delta(String),
    TotalTokens(u64),
    Done,
    Ignore,
}

/// Parse a single SSE line (already stripped of its trailing newline).
///
/// Tolerant by design: providers interleave keep-alive comments, empty
/// payloads and unknown fields, and none of that should break a chat.
pub fn parse_sse_line(line: &str) -> ChunkPiece {
    let trimmed = line.trim();
    if !trimmed.starts_with("data:") {
        return ChunkPiece::Ignore;
    }
    let payload = trimmed[5..].trim();
    if payload.is_empty() {
        return ChunkPiece::Ignore;
    }
    if payload == "[DONE]" {
        return ChunkPiece::Done;
    }
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(_) => return ChunkPiece::Ignore,
    };
    if let Some(text) = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|c| c.as_str())
    {
        if !text.is_empty() {
            return ChunkPiece::Delta(text.to_string());
        }
    }
    if let Some(total) = value
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|t| t.as_u64())
    {
        return ChunkPiece::TotalTokens(total);
    }
    ChunkPiece::Ignore
}

/// Extract an inline error message, if the stream carries one.
pub fn parse_stream_error(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("data:") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed[5..].trim()).ok()?;
    value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(|m| m.to_string())
}

// ------------------------------------------------------------------ events

/// Streamed to the webview over a Tauri `Channel`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatEvent {
    Delta { text: String },
    Done {
        tokens: u64,
        seconds: u64,
        estimated: bool,
    },
}

/// Hard stops for one response, derived from the remaining allowance. Keeping
/// the partial answer (and charging for it) is the honest behaviour: the tokens
/// were really spent, so discarding them would just hide the cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub tokens: u64,
    pub seconds: u64,
}

/// What a completed call should be charged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Charge {
    pub tokens: u64,
    pub seconds: u64,
    /// True when the provider didn't report usage and we estimated.
    pub estimated: bool,
    /// True when the stream was cut short at the allowance boundary.
    pub truncated: bool,
}

/// Result of the Settings → AI provider connection test.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestOutcome {
    pub ok: bool,
    pub model: String,
    pub tokens: u64,
    pub seconds: u64,
    pub message: String,
}

fn http_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECONDS))
        .build()
        .map_err(|e| AppError::Internal {
            message: format!("could not build http client: {e}"),
        })
}

/// Turn a non-success response into a typed error carrying the provider's own
/// message (truncated) — that text is usually the fastest path to a fix.
async fn http_error(response: reqwest::Response, url: &str) -> AppError {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|m| m.to_string())
        })
        .unwrap_or(body);
    let message: String = message.chars().take(300).collect();
    AppError::ProviderHttp {
        url: url.to_string(),
        status,
        message,
    }
}

/// Stream one completion, forwarding deltas through `on_event`.
///
/// Returns the [`Charge`] for the call. Callers own the entitlement preflight
/// and the debit — this function only talks to the provider.
pub async fn stream_chat<F>(
    config: &ProviderConfig,
    api_key: &str,
    system: &str,
    history: &[ChatTurn],
    message: &str,
    budget: Option<Budget>,
    mut on_event: F,
) -> Result<Charge, AppError>
where
    F: FnMut(ChatEvent),
{
    let endpoint = resolve_endpoint(config)?;
    let client = http_client()?;
    let body = build_body(&config.model, true, system, history, message);
    let started = Instant::now();

    let response = client
        .post(&endpoint)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Network {
            url: endpoint.clone(),
            message: e.to_string(),
        })?;

    if !response.status().is_success() {
        return Err(http_error(response, &endpoint).await);
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut text = String::new();
    let mut reported: Option<u64> = None;
    let mut truncated = false;

    // Spend so far, in whichever unit the budget caps.
    let over_budget = |text: &str, elapsed: Duration| match budget {
        None => false,
        Some(limit) => {
            let tokens = estimate_tokens(system)
                .saturating_add(estimate_tokens(message))
                .saturating_add(estimate_tokens(text));
            tokens >= limit.tokens || elapsed.as_secs() >= limit.seconds
        }
    };

    'stream: while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| AppError::Network {
            url: endpoint.clone(),
            message: format!("stream read failed: {e}"),
        })?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        // Frames are newline-delimited; keep the trailing partial line.
        let mut lines: Vec<String> = buffer
            .split('\n')
            .map(|l| l.to_string())
            .collect::<Vec<_>>();
        buffer = lines.pop().unwrap_or_default();

        for line in lines {
            if let Some(message) = parse_stream_error(&line) {
                return Err(AppError::ProviderStream { message });
            }
            match parse_sse_line(&line) {
                ChunkPiece::Delta(delta) => {
                    if text.chars().count() + delta.chars().count() > MAX_RESPONSE_CHARS {
                        return Err(AppError::ProviderStream {
                            message: "provider response exceeded the size limit".into(),
                        });
                    }
                    text.push_str(&delta);
                    on_event(ChatEvent::Delta {
                        text: delta.clone(),
                    });
                    if over_budget(&text, started.elapsed()) {
                        // Stop reading (dropping the stream closes the
                        // connection) and keep what arrived — the tokens were
                        // genuinely spent, so the ledger must record them.
                        truncated = true;
                        break 'stream;
                    }
                }
                ChunkPiece::TotalTokens(total) => reported = Some(total),
                ChunkPiece::Done | ChunkPiece::Ignore => {}
            }
        }
    }

    let seconds = elapsed_seconds(started.elapsed());
    let charge = Charge {
        tokens: reported.unwrap_or_else(|| {
            estimate_tokens(system).saturating_add(estimate_tokens(message)).saturating_add(estimate_tokens(&text))
        }),
        seconds,
        estimated: reported.is_none(),
        truncated,
    };
    on_event(ChatEvent::Done {
        tokens: charge.tokens,
        seconds: charge.seconds,
        estimated: charge.estimated,
    });
    Ok(charge)
}

/// Configure-time connectivity probe. Goes through the same URL gate and body
/// builder with a throwaway persona, and deliberately skips the license/meter
/// gate — nobody has usage to spend before setup finishes.
pub async fn test_connection(
    config: &ProviderConfig,
    api_key: &str,
) -> Result<TestOutcome, AppError> {
    let endpoint = resolve_endpoint(config)?;
    let client = http_client()?;
    let persona = system_prompt(
        "Connectivity probe",
        "You are a connectivity probe. Reply with the single word: ok.",
        "en",
    );
    let body = build_body(&config.model, false, &persona, &[], "Reply with the single word: ok.");
    let started = Instant::now();

    let response = client
        .post(&endpoint)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Network {
            url: endpoint.clone(),
            message: e.to_string(),
        })?;

    if !response.status().is_success() {
        return Err(http_error(response, &endpoint).await);
    }

    let payload: serde_json::Value = response.json().await.map_err(|e| AppError::ProviderStream {
        message: format!("could not read the test response: {e}"),
    })?;
    let reply = payload
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    let tokens = payload
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or_else(|| estimate_tokens(&reply));

    Ok(TestOutcome {
        ok: true,
        model: config.model.clone(),
        tokens,
        seconds: elapsed_seconds(started.elapsed()),
        message: reply.chars().take(120).collect(),
    })
}

/// Sub-second calls still bill as one second on the time meter.
fn elapsed_seconds(elapsed: Duration) -> u64 {
    elapsed.as_secs().max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_requires_an_agent_and_a_persona() {
        let missing_persona = r#"{"agentSlug":"sre","message":"hi"}"#;
        assert!(serde_json::from_str::<ChatRequest>(missing_persona).is_err());

        let full = r#"{"agentSlug":"sre","agentName":"SRE","persona":"body","message":"hi","lang":"fa"}"#;
        let request: ChatRequest = serde_json::from_str(full).expect("request");
        assert_eq!(request.agent_slug, "sre");
        assert_eq!(request.persona, "body");
        assert!(request.history.is_empty(), "history is optional");
    }

    #[test]
    fn public_hosts_pass_without_consent() {
        let url = validate_base_url("https://api.deepseek.com/v1/", false, &[]).expect("public");
        assert_eq!(url, "https://api.deepseek.com/v1", "trailing slash trimmed");
        assert!(validate_base_url("https://openrouter.ai/api/v1", false, &[]).is_ok());
    }

    #[test]
    fn private_hosts_need_an_exact_consent() {
        let local = "http://127.0.0.1:11434/v1";
        match validate_base_url(local, false, &[]) {
            Err(AppError::ProviderBlockedHost { host }) => assert_eq!(host, "127.0.0.1"),
            other => panic!("expected ProviderBlockedHost, got {other:?}"),
        }
        // Consent for a different host must not leak across.
        match validate_base_url(local, true, &["localhost".to_string()]) {
            Err(AppError::ProviderBlockedHost { .. }) => {}
            other => panic!("expected ProviderBlockedHost, got {other:?}"),
        }
        // Explicit, exact consent (case-insensitive) unlocks it.
        assert!(validate_base_url(local, true, &["127.0.0.1".to_string()]).is_ok());
        assert!(validate_base_url("http://LOCALHOST:11434/v1", true, &["localhost".into()]).is_ok());
        // The master switch off means consent is never consulted.
        match validate_base_url(local, false, &["127.0.0.1".to_string()]) {
            Err(AppError::ProviderBlockedHost { .. }) => {}
            other => panic!("expected ProviderBlockedHost, got {other:?}"),
        }
    }

    #[test]
    fn link_local_metadata_host_stays_blocked_even_with_consent_for_localhost() {
        match validate_base_url(
            "http://169.254.169.254/latest",
            true,
            &["localhost".to_string()],
        ) {
            Err(AppError::ProviderBlockedHost { host }) => assert_eq!(host, "169.254.169.254"),
            other => panic!("expected ProviderBlockedHost, got {other:?}"),
        }
    }

    #[test]
    fn malformed_base_urls_are_rejected() {
        assert!(matches!(
            validate_base_url("", false, &[]),
            Err(AppError::ProviderNotConfigured { .. })
        ));
        assert!(matches!(
            validate_base_url("ftp://example.com", false, &[]),
            Err(AppError::InvalidArgument { .. })
        ));
        assert!(matches!(
            validate_base_url("https://user:pass@example.com/v1", false, &[]),
            Err(AppError::InvalidArgument { .. })
        ));
        assert!(matches!(
            validate_base_url("not a url", false, &[]),
            Err(AppError::InvalidArgument { .. })
        ));
        assert!(matches!(
            validate_base_url(&format!("https://example.com/{}", "x".repeat(3000)), false, &[]),
            Err(AppError::InvalidArgument { .. })
        ));
    }

    #[test]
    fn resolve_endpoint_revalidates_the_stored_config() {
        let mut config = ProviderConfig {
            base_url: "http://127.0.0.1:11434/v1".into(),
            ..Default::default()
        };
        assert!(matches!(
            resolve_endpoint(&config),
            Err(AppError::ProviderBlockedHost { .. })
        ));
        config.allow_private_host = true;
        config.consented_hosts = vec!["127.0.0.1".into()];
        assert_eq!(
            resolve_endpoint(&config).expect("endpoint"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
    }

    #[test]
    fn config_defaults_deserialize_from_an_empty_object() {
        let config: ProviderConfig = serde_json::from_str("{}").expect("defaults");
        assert!(config.enabled);
        assert_eq!(config.meter, MeterMode::Tokens);
        assert_eq!(config.agents, vec!["*".to_string()]);
        assert!(!config.allow_private_host);
        assert_eq!(config, ProviderConfig::default());
    }

    #[test]
    fn config_ready_and_scope() {
        let mut config = ProviderConfig {
            base_url: "https://api.example.com/v1".into(),
            model: "deepseek-chat".into(),
            ..Default::default()
        };
        assert!(!config.ready(false), "a key is required");
        assert!(config.ready(true));
        assert!(config.covers_agent("anything"));

        config.agents = vec!["sre".into()];
        assert!(config.covers_agent("sre"));
        assert!(!config.covers_agent("frontend-developer"));

        config.enabled = false;
        assert!(!config.ready(true));
    }

    #[test]
    fn build_body_frames_the_persona_and_orders_messages() {
        let history = vec![
            ChatTurn {
                role: "user".into(),
                content: "hello".into(),
            },
            ChatTurn {
                role: "assistant".into(),
                content: "hi".into(),
            },
        ];
        let body = build_body("m", true, "PERSONA", &history, "latest");
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "PERSONA");
        assert_eq!(messages[1]["content"], "hello");
        assert_eq!(messages[2]["content"], "hi");
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"], "latest");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn build_body_truncates_history_and_omits_stream_options_when_not_streaming() {
        let history: Vec<ChatTurn> = (0..25)
            .map(|i| ChatTurn {
                role: "user".into(),
                content: format!("turn-{i}"),
            })
            .collect();
        let body = build_body("m", false, "P", &history, "now");
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 1 + HISTORY_LIMIT + 1);
        assert_eq!(messages[1]["content"], "turn-15", "last 10 turns kept");
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn system_prompt_always_carries_the_persona_and_language() {
        let prompt = system_prompt("SRE", "You keep systems up.", "fa");
        assert!(prompt.contains("SRE"));
        assert!(prompt.contains("You keep systems up."));
        assert!(prompt.contains("Persian"));

        let en = system_prompt("SRE", "", "en");
        assert!(en.contains("user's language"));
        assert!(!en.contains("\n\n\n"), "no empty persona block");
    }

    #[test]
    fn sse_lines_parse_into_deltas_usage_and_done() {
        assert_eq!(
            parse_sse_line("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}"),
            ChunkPiece::Delta("hi".into())
        );
        assert_eq!(
            parse_sse_line("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}"),
            ChunkPiece::Ignore,
            "a role-only delta carries no text"
        );
        assert_eq!(
            parse_sse_line("data: {\"usage\":{\"total_tokens\":123}}"),
            ChunkPiece::TotalTokens(123)
        );
        assert_eq!(parse_sse_line("data: [DONE]"), ChunkPiece::Done);
        assert_eq!(parse_sse_line(": keep-alive"), ChunkPiece::Ignore);
        assert_eq!(parse_sse_line("data: {broken"), ChunkPiece::Ignore);
        assert_eq!(parse_sse_line(""), ChunkPiece::Ignore);
    }

    #[test]
    fn inline_stream_errors_are_surfaced() {
        assert_eq!(
            parse_stream_error("data: {\"error\":{\"message\":\"quota exceeded\"}}").as_deref(),
            Some("quota exceeded")
        );
        assert_eq!(parse_stream_error("data: {\"choices\":[]}"), None);
        assert_eq!(parse_stream_error("event: ping"), None);
    }

    #[test]
    fn sub_second_calls_bill_one_second() {
        assert_eq!(elapsed_seconds(Duration::from_millis(10)), 1);
        assert_eq!(elapsed_seconds(Duration::from_millis(1500)), 1);
        assert_eq!(elapsed_seconds(Duration::from_secs(3)), 3);
    }
}
