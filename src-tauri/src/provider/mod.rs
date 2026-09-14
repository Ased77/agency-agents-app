//! Custom (non-Claude) AI provider + offline paid access.
//!
//! The app ships no inference of its own. Users who want to chat with a
//! catalog agent from inside the app point it at **their own** OpenAI-compatible
//! endpoint and paste a signed license; the provider is deliberately not
//! Claude/Anthropic, and agent chats are the only thing it is ever used for.
//!
//! Module map (mirrors `memory-bank/systemPatterns.md` §6):
//!
//! | module | role |
//! |---|---|
//! | [`openai`] | provider config, SSRF gate, SSE streaming client |
//! | [`license`] | signed payload format + offline verification |
//! | [`meter`] | consumption ledger, entitlement math, clock guard |
//!
//! The command surface lives in [`crate::commands::provider`]; the license
//! public key is pinned in `lib.rs` next to the updater's.

pub mod license;
pub mod meter;
pub mod openai;
