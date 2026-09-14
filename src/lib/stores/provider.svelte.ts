/**
 * Provider store — the renderer's view of the custom (non-Claude) AI provider.
 *
 * Mirrors `state/provider.json` + the keyring-backed license, exactly like the
 * settings store mirrors `settings.json`: the backend file is authoritative,
 * this class is a reactive cache. Nothing secret passes through here — the API
 * key is written once and never read back, and the license only ever surfaces
 * as a verified status.
 *
 * "Agents only" is enforced backend-side; the renderer's job is to gate the UI
 * (hide the composer when the license can't cover the agent) so a user never
 * types a message that was always going to be refused.
 */

import {
  entitlementGet,
  licenseClear,
  licenseSet,
  licenseStatus,
  providerConfigGet,
  providerConfigSet,
  providerKeyClear,
  providerKeySet,
  providerKeyStatus,
  providerTest,
} from "$lib/api";
import {
  isAppError,
  PROVIDER_CONFIG_DEFAULTS,
  type Entitlement,
  type LicenseStatus,
  type ProviderConfig,
  type ProviderTestOutcome,
} from "$lib/types";

class ProviderStore {
  /** Authoritative provider settings, or `null` until first load. */
  data: ProviderConfig | null = $state(null);

  /** Whether an API key is in the keyring. */
  hasKey: boolean = $state(false);

  /** Verified license, or `null` until first load. */
  license: LicenseStatus | null = $state(null);

  /** Remaining allowance, when licensed. */
  entitlement: Entitlement | null = $state(null);

  /** True while a load/save/test is in flight. */
  loading: boolean = $state(false);

  /** Last error, as a human-readable string. Cleared on success. */
  error: string | null = $state(null);

  /** Last connection-test result. */
  testResult: ProviderTestOutcome | null = $state(null);

  /** Load everything the Settings section and the chat pane need. Safe to
      call from multiple mount points. */
  async load(): Promise<void> {
    this.loading = true;
    this.error = null;
    try {
      const [config, key, license] = await Promise.all([
        providerConfigGet(),
        providerKeyStatus(),
        licenseStatus(),
      ]);
      this.data = config;
      this.hasKey = key.configured;
      this.license = license;
      this.entitlement = license.valid ? await entitlementGet() : null;
    } catch (e) {
      this.error = describe(e);
      // Render defaults rather than nothing so the form stays usable when
      // `provider.json` is unreadable (the backend fails loudly on corruption).
      this.data = this.data ?? { ...PROVIDER_CONFIG_DEFAULTS };
    } finally {
      this.loading = false;
    }
  }

  /** The effective config: loaded value, or defaults. */
  get effective(): ProviderConfig {
    return this.data ?? { ...PROVIDER_CONFIG_DEFAULTS };
  }

  /** Whether the provider could serve a call right now (ignoring license). */
  get configured(): boolean {
    const c = this.effective;
    return (
      c.enabled &&
      c.baseUrl.trim() !== "" &&
      c.model.trim() !== "" &&
      this.hasKey &&
      c.agents.length > 0
    );
  }

  /** Whether a verified, unexpired license is in place. */
  get licensed(): boolean {
    return this.license?.valid === true;
  }

  /** Whether this agent may chat: provider ready + license covers the slug. */
  coversAgent(slug: string): boolean {
    if (!this.configured || !this.licensed) return false;
    const scope = this.effective.agents;
    const licenseScope = this.license?.agents ?? [];
    const inScope = scope.includes("*") || scope.includes(slug);
    const inLicense = licenseScope.includes("*") || licenseScope.includes(slug);
    return inScope && inLicense;
  }

  async save(partial: Partial<ProviderConfig>): Promise<boolean> {
    this.loading = true;
    this.error = null;
    try {
      this.data = await providerConfigSet({ ...this.effective, ...partial });
      return true;
    } catch (e) {
      this.error = describe(e);
      return false;
    } finally {
      this.loading = false;
    }
  }

  async setKey(key: string): Promise<boolean> {
    this.error = null;
    try {
      await providerKeySet(key);
      this.hasKey = true;
      return true;
    } catch (e) {
      this.error = describe(e);
      return false;
    }
  }

  async clearKey(): Promise<void> {
    this.error = null;
    try {
      await providerKeyClear();
      this.hasKey = false;
    } catch (e) {
      this.error = describe(e);
    }
  }

  /** Verify + store a pasted license. */
  async setLicense(blob: string): Promise<boolean> {
    this.loading = true;
    this.error = null;
    try {
      this.license = await licenseSet(blob);
      this.entitlement = await entitlementGet();
      return this.license.valid;
    } catch (e) {
      this.error = describe(e);
      this.license = await licenseStatus().catch(() => this.license);
      return false;
    } finally {
      this.loading = false;
    }
  }

  async clearLicense(): Promise<void> {
    this.error = null;
    try {
      this.license = await licenseClear();
      this.entitlement = null;
    } catch (e) {
      this.error = describe(e);
    }
  }

  async test(): Promise<void> {
    this.loading = true;
    this.error = null;
    this.testResult = null;
    try {
      this.testResult = await providerTest();
    } catch (e) {
      this.error = describe(e);
    } finally {
      this.loading = false;
    }
  }

  /** Re-read the allowance after a chat debits it. */
  async refreshEntitlement(): Promise<void> {
    if (!this.licensed) return;
    try {
      this.entitlement = await entitlementGet();
    } catch {
      // A stale allowance is cosmetic; keep the last good value.
    }
  }
}

/** Turn any thrown value into readable text. Backend errors are already
    shaped for humans by `appErrorMessage`. */
function describe(e: unknown): string {
  if (isAppError(e)) {
    if (e.code === "license_invalid") return `License not usable (${e.reason}): ${e.message}`;
    if (e.code === "provider_blocked_host") return `${e.host} is not a public host`;
    if (e.code === "provider_http") return `HTTP ${e.status}: ${e.message}`;
    if ("message" in e && typeof e.message === "string") return e.message;
    return e.code;
  }
  return String(e);
}

export const provider = new ProviderStore();
