<script lang="ts">
  /**
   * SettingsSectionProvider.svelte — the custom (non-Claude) AI provider.
   *
   * Everything a user needs to make agent chats work, in one place:
   *
   *  1. **Endpoint** — base URL + model of their own OpenAI-compatible
   *     provider. No Claude/Anthropic anywhere; that is the point of the
   *     feature.
   *  2. **Credential** — the API key, written straight to the OS keyring and
   *     never read back into the renderer.
   *  3. **Private endpoints** — loopback / RFC1918 hosts (Ollama, LM Studio,
   *     an internal gateway) are refused by the SSRF guard unless the user
   *     explicitly consents to that exact host here.
   *  4. **License** — the signed, offline-verified blob that unlocks paid
   *     access, plus the remaining allowance it grants.
   *
   * Agent scope is deliberately configurable but promotively broad: an empty
   * list means "any licensed agent", and a license can itself narrow further.
   * A chat is only ever allowed where BOTH agree.
   */

  import { onMount } from "svelte";
  import KeyRound from "@lucide/svelte/icons/key-round";
  import Plug from "@lucide/svelte/icons/plug";
  import ShieldCheck from "@lucide/svelte/icons/shield-check";
  import ShieldAlert from "@lucide/svelte/icons/shield-alert";
  import BadgeCheck from "@lucide/svelte/icons/badge-check";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import { provider } from "$lib/stores/provider.svelte";
  import { i18n } from "$lib/stores/i18n.svelte";
  import {
    PROVIDER_CONFIG_DEFAULTS,
    type ProviderConfig,
    type ProviderMeter,
  } from "$lib/types";

  let draft = $state<ProviderConfig>({ ...PROVIDER_CONFIG_DEFAULTS });
  let scopeText = $state("");
  let hostsText = $state("");
  let keyInput = $state("");
  let licenseInput = $state("");
  let saved = $state(false);

  onMount(() => {
    void provider.load();
  });

  // Adopt whatever the backend handed us (load or save canonicalizes).
  $effect(() => {
    const data = provider.data;
    if (!data) return;
    draft = { ...data };
    scopeText = data.agents.includes("*") ? "" : data.agents.join(", ");
    hostsText = data.consentedHosts.join(", ");
  });

  const meters: ProviderMeter[] = ["tokens", "time", "both"];

  const licenseValid = $derived(provider.license?.valid === true);
  const entitlement = $derived(provider.entitlement);

  function parseList(text: string): string[] {
    return text
      .split(",")
      .map((entry) => entry.trim())
      .filter(Boolean);
  }

  async function save() {
    saved = false;
    const ok = await provider.save({
      ...draft,
      agents: scopeText.trim() === "" ? ["*"] : parseList(scopeText),
      consentedHosts: parseList(hostsText).map((host) => host.toLowerCase()),
    });
    saved = ok;
  }

  /** Consent to the host of the URL currently typed, without the user having
   *  to retype it — the exact host is what the SSRF guard checks. */
  function addCurrentHost() {
    try {
      const host = new URL(draft.baseUrl).hostname.toLowerCase();
      if (!host) return;
      const hosts = new Set(parseList(hostsText).map((h) => h.toLowerCase()));
      hosts.add(host);
      hostsText = [...hosts].join(", ");
    } catch {
      // A malformed URL surfaces on save; nothing to add here.
    }
  }

  async function saveKey() {
    if (keyInput.trim() === "") return;
    if (await provider.setKey(keyInput)) keyInput = "";
  }

  async function activateLicense() {
    if (licenseInput.trim() === "") return;
    if (await provider.setLicense(licenseInput)) licenseInput = "";
  }

  function formatMinutes(seconds: number): string {
    return new Intl.NumberFormat(i18n.locale).format(Math.max(0, Math.round(seconds / 60)));
  }

  function formatCount(value: number): string {
    return new Intl.NumberFormat(i18n.locale).format(value);
  }

  function formatExpiry(unix: number | null | undefined): string {
    if (!unix) return i18n.t("provider.license.never");
    return new Date(unix * 1000).toLocaleDateString(i18n.locale);
  }
</script>

<div class="section">
  <h2>{i18n.t("settings.provider")}</h2>
  <p class="hint">{i18n.t("provider.intro")}</p>

  <!-- ---------------- endpoint ---------------- -->
  <div class="field">
    <label class="label" for="prov-label">{i18n.t("provider.field.label")}</label>
    <input id="prov-label" type="text" bind:value={draft.label} placeholder={i18n.t("provider.field.labelHint")} />
  </div>

  <div class="field">
    <label class="label" for="prov-url">{i18n.t("provider.field.baseUrl")}</label>
    <input id="prov-url" type="url" bind:value={draft.baseUrl} placeholder="https://api.example.com/v1" spellcheck="false" />
    <p class="hint">{i18n.t("provider.field.baseUrlHint")}</p>
  </div>

  <div class="field">
    <label class="label" for="prov-model">{i18n.t("provider.field.model")}</label>
    <input id="prov-model" type="text" bind:value={draft.model} placeholder="deepseek-chat" spellcheck="false" />
  </div>

  <div class="field">
    <span class="label">{i18n.t("provider.field.meter")}</span>
    <div class="segmented" role="radiogroup" aria-label={i18n.t("provider.field.meter")}>
      {#each meters as option (option)}
        <button
          type="button"
          class="segment"
          class:active={draft.meter === option}
          role="radio"
          aria-checked={draft.meter === option}
          onclick={() => (draft.meter = option)}
        >
          {i18n.t(`provider.meter.${option}` as "provider.meter.tokens")}
        </button>
      {/each}
    </div>
    <p class="hint">{i18n.t("provider.field.meterHint")}</p>
  </div>

  <div class="rates">
    <div class="field">
      <label class="label" for="prov-rate-tokens">{i18n.t("provider.rate.tokens")}</label>
      <input id="prov-rate-tokens" type="number" min="0" bind:value={draft.tomanPer1kTokens} />
    </div>
    <div class="field">
      <label class="label" for="prov-rate-time">{i18n.t("provider.rate.time")}</label>
      <input id="prov-rate-time" type="number" min="0" bind:value={draft.tomanPerMinute} />
    </div>
  </div>

  <div class="field">
    <label class="label" for="prov-scope">{i18n.t("provider.field.scope")}</label>
    <input
      id="prov-scope"
      type="text"
      bind:value={scopeText}
      placeholder="sre, frontend-developer"
      spellcheck="false"
    />
    <p class="hint">{i18n.t("provider.field.scopeHint")}</p>
  </div>

  <div class="field">
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.enabled}
        onchange={(e) => (draft.enabled = (e.currentTarget as HTMLInputElement).checked)}
      />
      <span class="toggle-track" aria-hidden="true"></span>
      <span class="toggle-label">{i18n.t("provider.enabled")}</span>
    </label>
  </div>

  <!-- ---------------- credential ---------------- -->
  <div class="block">
    <div class="block-head">
      <KeyRound size={16} />
      <strong>{i18n.t("provider.key.title")}</strong>
      <span class="pill" class:on={provider.hasKey}>
        {provider.hasKey ? i18n.t("provider.key.set") : i18n.t("provider.key.unset")}
      </span>
    </div>
    <p class="hint">{i18n.t("provider.key.hint")}</p>
    <div class="row">
      <input
        type="password"
        bind:value={keyInput}
        placeholder={i18n.t("provider.key.placeholder")}
        autocomplete="off"
        spellcheck="false"
      />
      <button class="btn-secondary" onclick={() => void saveKey()} disabled={keyInput.trim() === ""}>
        {i18n.t("provider.key.save")}
      </button>
      {#if provider.hasKey}
        <button class="btn-secondary" onclick={() => void provider.clearKey()}>
          {i18n.t("provider.key.remove")}
        </button>
      {/if}
    </div>
  </div>

  <!-- ---------------- private endpoints ---------------- -->
  <div class="block">
    <div class="block-head">
      {#if draft.allowPrivateHost}
        <ShieldAlert size={16} />
      {:else}
        <ShieldCheck size={16} />
      {/if}
      <strong>{i18n.t("provider.private.title")}</strong>
    </div>
    <p class="hint">{i18n.t("provider.private.hint")}</p>
    <label class="toggle">
      <input
        type="checkbox"
        checked={draft.allowPrivateHost}
        onchange={(e) => (draft.allowPrivateHost = (e.currentTarget as HTMLInputElement).checked)}
      />
      <span class="toggle-track" aria-hidden="true"></span>
      <span class="toggle-label">{i18n.t("provider.private.enable")}</span>
    </label>
    <div class="field">
      <label class="label" for="prov-hosts">{i18n.t("provider.private.hosts")}</label>
      <div class="row">
        <input id="prov-hosts" type="text" bind:value={hostsText} placeholder="127.0.0.1" spellcheck="false" />
        <button class="btn-secondary" onclick={addCurrentHost}>{i18n.t("provider.private.addHost")}</button>
      </div>
    </div>
  </div>

  <!-- ---------------- license ---------------- -->
  <div class="block">
    <div class="block-head">
      <BadgeCheck size={16} />
      <strong>{i18n.t("provider.license.title")}</strong>
      <span class="pill" class:on={licenseValid}>
        {licenseValid ? i18n.t("provider.license.valid") : i18n.t("provider.license.invalid")}
      </span>
    </div>
    <p class="hint">{i18n.t("provider.license.hint")}</p>

    {#if provider.license && !licenseValid && provider.license.reason}
      <div class="callout warn" role="alert">
        <TriangleAlert size={16} />
        <span>
          {i18n.t("provider.license.reason", { reason: provider.license.reason })}
          {provider.license.message ?? ""}
        </span>
      </div>
    {/if}

    {#if licenseValid && entitlement}
      <dl class="facts">
        <div><dt>{i18n.t("provider.license.plan")}</dt><dd>{entitlement.plan}</dd></div>
        <div><dt>{i18n.t("provider.license.meter")}</dt><dd>{i18n.t(`provider.meter.${entitlement.meter}` as "provider.meter.tokens")}</dd></div>
        <div><dt>{i18n.t("provider.license.expires")}</dt><dd>{formatExpiry(entitlement.expiresAt)}</dd></div>
        <div>
          <dt>{i18n.t("provider.license.remaining")}</dt>
          <dd>
            {#if entitlement.meter !== "time"}
              {formatCount(entitlement.tokensRemaining)} {i18n.t("agentChat.unit.tokens")}
            {/if}
            {#if entitlement.meter === "both"} · {/if}
            {#if entitlement.meter !== "tokens"}
              {formatMinutes(entitlement.secondsRemaining)} {i18n.t("agentChat.unit.minutes")}
            {/if}
          </dd>
        </div>
      </dl>
    {/if}

    <div class="field">
      <label class="label" for="prov-license">{i18n.t("provider.license.paste")}</label>
      <textarea
        id="prov-license"
        rows="3"
        bind:value={licenseInput}
        placeholder={i18n.t("provider.license.placeholder")}
        spellcheck="false"
      ></textarea>
    </div>
    <div class="row">
      <button class="btn-primary" onclick={() => void activateLicense()} disabled={licenseInput.trim() === ""}>
        {i18n.t("provider.license.activate")}
      </button>
      {#if provider.license?.present}
        <button class="btn-secondary" onclick={() => void provider.clearLicense()}>
          {i18n.t("provider.license.remove")}
        </button>
      {/if}
    </div>
  </div>

  <!-- ---------------- actions ---------------- -->
  <div class="row">
    <button class="btn-primary" onclick={() => void save()} disabled={provider.loading}>
      {i18n.t("provider.save")}
    </button>
    <button class="btn-secondary" onclick={() => void provider.test()} disabled={provider.loading || !provider.configured}>
      <Plug size={14} /> {provider.loading ? i18n.t("provider.test.running") : i18n.t("provider.test.run")}
    </button>
    {#if saved}<span class="ok">{i18n.t("provider.saved")}</span>{/if}
  </div>

  {#if provider.testResult}
    <p class="hint">
      {i18n.t("provider.test.ok", {
        model: provider.testResult.model,
        tokens: formatCount(provider.testResult.tokens),
        seconds: formatCount(provider.testResult.seconds),
      })}
    </p>
  {/if}

  {#if provider.error}
    <div class="callout warn" role="alert">
      <TriangleAlert size={16} />
      <span>{provider.error}</span>
    </div>
  {/if}

  <p class="hint">{i18n.t("provider.footnote")}</p>
</div>

<style>
  .section { display: flex; flex-direction: column; gap: var(--space-4); max-width: 580px; }
  h2 {
    font-size: var(--text-h1);
    font-weight: var(--fw-semibold);
    color: var(--color-text-primary);
    margin-bottom: var(--space-2);
  }
  .field { display: flex; flex-direction: column; gap: var(--space-2); }
  .label { font-size: var(--text-body-sm); font-weight: var(--fw-medium); color: var(--color-text-primary); }
  .hint { font-size: var(--text-body-sm); color: var(--color-text-muted); line-height: var(--lh-snug); margin: 0; }
  input[type="text"],
  input[type="url"],
  input[type="password"],
  input[type="number"],
  textarea {
    padding: 6px 10px;
    border-radius: var(--radius-md);
    border: 1px solid var(--color-border);
    background: var(--color-surface-raised);
    color: var(--color-text-primary);
    font: inherit;
    font-size: var(--text-body-sm);
    width: 100%;
  }
  input:focus-visible,
  textarea:focus-visible { outline: 2px solid var(--color-border-focus, var(--color-brand)); outline-offset: 1px; }
  textarea { resize: vertical; font-family: var(--font-mono, ui-monospace, monospace); }
  .rates { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-3); }

  .segmented { display: inline-flex; border: 1px solid var(--color-border); border-radius: var(--radius-md); overflow: hidden; width: max-content; }
  .segment {
    padding: 6px 12px;
    background: var(--color-surface-raised);
    color: var(--color-text-secondary);
    border: 0;
    font-size: var(--text-body-sm);
    cursor: pointer;
  }
  .segment.active { background: var(--color-brand-subtle); color: var(--color-text-primary); font-weight: var(--fw-medium); }

  .block {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-surface-sunken);
  }
  .block-head { display: inline-flex; align-items: center; gap: var(--space-2); color: var(--color-text-primary); }
  .row { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
  .row input { flex: 1 1 12rem; }

  .pill {
    margin-inline-start: auto;
    padding: 2px 8px;
    border-radius: var(--radius-full);
    font-size: var(--text-caption);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    color: var(--color-text-muted);
  }
  .pill.on { background: var(--color-success-subtle); border-color: var(--color-success); color: var(--color-success-on-subtle); }

  .facts { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-2) var(--space-4); margin: 0; }
  .facts div { display: flex; flex-direction: column; }
  .facts dt { font-size: var(--text-caption); color: var(--color-text-muted); }
  .facts dd { margin: 0; font-size: var(--text-body-sm); color: var(--color-text-primary); }

  .btn-primary,
  .btn-secondary {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border-radius: var(--radius-md);
    font-size: var(--text-body-sm);
    font-weight: var(--fw-medium);
    cursor: pointer;
    width: max-content;
  }
  .btn-primary { background: var(--color-accent, #b8542a); color: white; }
  .btn-primary:hover:not(:disabled) { filter: brightness(1.05); }
  .btn-primary:disabled { opacity: 0.6; cursor: not-allowed; }
  .btn-secondary { background: var(--color-surface-raised); color: var(--color-text-primary); border: 1px solid var(--color-border); }
  .btn-secondary:disabled { opacity: 0.6; cursor: not-allowed; }

  .toggle { display: inline-flex; align-items: center; gap: var(--space-2); cursor: pointer; user-select: none; }
  .toggle input { position: absolute; opacity: 0; pointer-events: none; }
  .toggle-track {
    width: 36px; height: 20px;
    background: var(--color-surface-sunken);
    border: 1px solid var(--color-border);
    border-radius: 999px;
    position: relative;
    transition: background-color var(--motion-duration-fast) var(--motion-ease-out);
  }
  .toggle-track::after {
    content: "";
    position: absolute;
    top: 1px; left: 1px;
    width: 16px; height: 16px;
    background: var(--color-surface-raised);
    border-radius: 50%;
    box-shadow: var(--shadow-xs);
    transition: transform var(--motion-duration-fast) var(--motion-ease-out);
  }
  .toggle input:checked + .toggle-track { background: var(--color-accent, #b8542a); border-color: var(--color-accent, #b8542a); }
  .toggle input:checked + .toggle-track::after { transform: translateX(16px); background: white; }
  .toggle-label { font-size: var(--text-body); font-weight: var(--fw-medium); color: var(--color-text-primary); }

  .callout.warn {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    background: var(--color-warning-subtle, var(--color-surface-sunken));
    color: var(--color-text-primary);
    font-size: var(--text-body-sm);
    border: 1px solid var(--color-warning, var(--color-border));
  }
  .ok { color: var(--color-success); font-size: var(--text-body-sm); }
</style>
