# AI provider licenses — issuance runbook

The custom (non-Claude) AI provider is a paid feature: agent chats stream from
**the user's own** OpenAI-compatible endpoint, and access is unlocked by a signed
license that the app verifies **offline**. There is no account system, no
activation server, and no network call involved in verification.

- Format + verification: `src-tauri/src/provider/license.rs`
- Ledger + clock guard: `src-tauri/src/provider/meter.rs`
- Endpoint + SSRF gate: `src-tauri/src/provider/openai.rs`
- Runtime ADR: `memory-bank/decisions.md` (2026-09-14)

## 1. Keypair (once, per issuer)

```bash
minisign -G -p ~/.config/agency-agents-app/license.pub \
            -s ~/.config/agency-agents-app/license.key
chmod 600 ~/.config/agency-agents-app/license.key
```

Use a **separate keypair from the updater's**. A leaked updater key must not be
able to mint licenses, and license issuance may be delegated per-customer
without touching release signing.

Then paste the **single-line base64** public key from `license.pub` (drop the
`untrusted comment:` line) into `LICENSE_PUBKEY` in `src-tauri/src/lib.rs` and
cut a release. Until that is set the feature fails closed: `license_set` refuses
every blob with `unconfigured`, so no build can accept a license it cannot
verify.

## 2. Payload

```json
{
  "v": 1,
  "licenseId": "cus_1024_2026-09",
  "plan": "pro",
  "meter": "tokens",
  "tokens": 500000,
  "minutes": 3000,
  "agents": ["*"],
  "expiresAt": 1790000000
}
```

| Field | Meaning |
|---|---|
| `meter` | `tokens`, `time`, or `both`. The provider profile's meter must be allowed here or every call is refused. |
| `tokens` | Token allowance for the token meter. |
| `minutes` | Time allowance (converted to seconds internally) for the time meter. |
| `agents` | Agent **slugs** this license unlocks. `["*"]` = every agent. A chat is allowed only where this list **and** the provider profile's `agents` list both cover the agent. |
| `expiresAt` | Unix seconds. `0` = never expires. |

Keep the JSON byte-stable once signed — the signature covers the raw payload
bytes, and the base64 line in the blob is those same bytes.

## 3. Sign and assemble

```bash
# Prehashed is the modern default; the app accepts both prehashed and classic.
minisign -SHm payload.json -s ~/.config/agency-agents-app/license.key

node scripts/assemble-license.mjs payload.json payload.json.minisig > license.txt
```

`license.txt` is what the customer pastes into **Settings → AI provider →
License**. The script never touches the private key; it only base64s the payload
and appends the four signature lines.

## 4. What the app enforces (and what it cannot)

Enforced locally, in this order, on every chat turn:

1. **Offline Mode** — `require_network("provider_chat")` blocks all outbound
   calls, license or not.
2. **Agents only** — the request must name an agent and carry its persona; the
   profile's `agents` list, then the license's `agents` list, must cover it.
3. **Allowance** — a pre-check reserves the estimate (or the one-minute
   minimum) and the post-call debit is true'd up; over-consumption clamps at
   zero.
4. **Clock** — `state/provider-usage.json` keeps a high-water mark, so moving
   the system clock backwards past it invalidates the license (`reason:
   "clock"`) rather than resurrecting an expired one.

Not enforceable, by design:

- **Revocation.** A license is valid until it expires. Issue short-lived
  licenses (90 days is a good default) and replace rather than revoke.
- **Machine binding.** The blob is not tied to a device, so it can be copied.
  Metering is per-machine (`state/provider-usage.json`), not global.
- **Client tampering.** A user who can edit their own app-data files can reset
  their local ledger. That is the standard trade-off for offline entitlements;
  if it becomes a problem, entitlements move server-side (a real backend, not a
  hand-rolled check).

## 5. Sanity check before issuing

```bash
cd src-tauri && cargo test --lib provider
```

The tests cover the verification path (fixture signature, tamper rejection,
placeholder-key fail-closed), the meter math, the clock guard, and the SSRF
consent rules — including that consenting to `localhost` does **not** unlock
`169.254.169.254`.
