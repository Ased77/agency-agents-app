<script lang="ts">
  /**
   * ChatPane — "Chat with this agent", mounted in the agent detail pane.
   *
   * This is the app's only inference surface, and it is agent-scoped by
   * construction: the request carries the agent's slug and its corpus persona,
   * the backend re-checks that the profile *and* the license both cover that
   * agent, and it debits the profile's meter (tokens or time) from the signed
   * allowance. Nothing here can produce a general-purpose chat.
   *
   * Collapsed by default: the workspace is a catalog first, a chat second.
   */
  import { onMount } from "svelte";
  import MessageSquare from "@lucide/svelte/icons/message-square";
  import Send from "@lucide/svelte/icons/send";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import Lock from "@lucide/svelte/icons/lock";
  import SettingsIcon from "@lucide/svelte/icons/settings";
  import { providerChatStream } from "$lib/api";
  import { provider } from "$lib/stores/provider.svelte";
  import { i18n } from "$lib/stores/i18n.svelte";
  import { ui } from "$lib/stores/ui.svelte";
  import { renderMarkdown } from "$lib/util/markdown";
  import type { Agent, ProviderChatRequest } from "$lib/types";

  let { agent }: { agent: Agent } = $props();

  /** One rendered chat turn. `streaming` marks the bubble chunks land in. */
  type Turn = {
    role: "user" | "assistant";
    content: string;
    tokens?: number;
    seconds?: number;
    estimated?: boolean;
    streaming?: boolean;
  };

  let open = $state(false);
  let input = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let turns = $state<Turn[]>([]);

  /** Bumped on clear so late events from a previous run are dropped. */
  let runId = 0;

  onMount(() => {
    if (provider.data === null) void provider.load();
  });

  const lang = $derived(i18n.locale === "fa" ? "fa" : "en");
  const covered = $derived(provider.coversAgent(agent.slug));
  const persona = $derived(agent.body?.trim() ?? "");
  const meter = $derived(provider.effective.meter);

  /** Minutes for display; the ledger itself is always seconds. */
  function formatMinutes(seconds: number): string {
    return formatCount(Math.max(0, Math.round(seconds / 60)));
  }

  function formatCount(value: number): string {
    return new Intl.NumberFormat(i18n.locale).format(value);
  }

  const meterLabel = $derived(
    meter === "both"
      ? i18n.t("agentChat.meter.both")
      : meter === "time"
        ? i18n.t("agentChat.meter.time")
        : i18n.t("agentChat.meter.tokens"),
  );

  const remaining = $derived.by(() => {
    const tokens = formatCount(provider.entitlement?.tokensRemaining ?? 0);
    const minutes = formatMinutes(provider.entitlement?.secondsRemaining ?? 0);
    if (meter === "time") return `${minutes} ${i18n.t("agentChat.unit.minutes")}`;
    if (meter === "both")
      return `${tokens} ${i18n.t("agentChat.unit.tokens")} · ${minutes} ${i18n.t("agentChat.unit.minutes")}`;
    return `${tokens} ${i18n.t("agentChat.unit.tokens")}`;
  });

  /** Context for the next request — read *before* a turn is appended, so the
      message being sent is never also present in the history. */
  function priorHistory(): { role: "user" | "assistant"; content: string }[] {
    return turns
      .filter((turn) => !turn.streaming && turn.content.trim() !== "")
      .slice(-10)
      .map((turn) => ({ role: turn.role, content: turn.content }));
  }

  function clear() {
    runId += 1;
    turns = [];
    error = null;
  }

  async function send() {
    const message = input.trim();
    if (!message || busy || !covered || !persona) return;

    const myRun = runId;
    const history = priorHistory();
    input = "";
    error = null;
    busy = true;
    turns = [
      ...turns,
      { role: "user", content: message },
      { role: "assistant", content: "", streaming: true },
    ];

    const request: ProviderChatRequest = {
      agentSlug: agent.slug,
      agentName: agent.name,
      persona,
      history,
      message,
      lang,
    };

    try {
      const charge = await providerChatStream(request, (event) => {
        if (myRun !== runId) return;
        if (event.kind !== "delta") return;
        const last = turns[turns.length - 1];
        if (!last || !last.streaming) return;
        turns = [...turns.slice(0, -1), { ...last, content: last.content + event.text }];
      });
      if (myRun === runId) {
        const last = turns[turns.length - 1];
        if (last?.streaming) {
          turns = [
            ...turns.slice(0, -1),
            {
              ...last,
              streaming: false,
              tokens: meter === "time" ? undefined : charge.tokens,
              seconds: meter === "time" ? charge.seconds : undefined,
              estimated: charge.estimated,
            },
          ];
        }
      }
      if (charge.truncated && myRun === runId) error = i18n.t("agentChat.truncated");
      await provider.refreshEntitlement();
    } catch (e) {
      if (myRun === runId) {
        // Keep a partial answer; drop an empty bubble.
        const last = turns[turns.length - 1];
        if (last?.streaming) {
          turns = last.content
            ? [...turns.slice(0, -1), { ...last, streaming: false }]
            : turns.slice(0, -1);
        }
        error = describeError(e);
      }
    } finally {
      if (myRun === runId) busy = false;
    }
  }

  /** Backend errors are already human-shaped; this keeps the pane honest even
      if one arrives as a bare object. */
  function describeError(e: unknown): string {
    if (typeof e === "object" && e !== null && "code" in e) {
      const code = (e as { code: string }).code;
      const message = (e as { message?: string }).message;
      const meterKey = (e as { meter?: string }).meter;
      if (code === "not_entitled" && meterKey)
        return i18n.t("agentChat.gate.outOfAllowance", { meter: meterKey });
      if (message) return message;
      return code;
    }
    return String(e);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  }
</script>

<section class="chat" aria-label={i18n.t("agentChat.title")}>
  <button
    class="chat-toggle"
    onclick={() => (open = !open)}
    aria-expanded={open}
  >
    <MessageSquare size={14} />
    <span class="chat-toggle-label">{i18n.t("agentChat.title")}</span>
    {#if open}
      <span class="chat-toggle-hint">{i18n.t("agentChat.collapse")}</span>
    {:else}
      <span class="chat-toggle-hint">
        {provider.configured
          ? i18n.t("agentChat.subtitle", { model: provider.effective.model })
          : i18n.t("agentChat.needsSetup")}
      </span>
    {/if}
  </button>

  {#if open}
    {#if !provider.configured}
      <div class="gate">
        <p>{i18n.t("agentChat.gate.notConfigured")}</p>
        <button class="gate-btn" onclick={() => ui.openSettings("provider")}>
          <SettingsIcon size={13} /> {i18n.t("agentChat.gate.openSettings")}
        </button>
      </div>
    {:else if !provider.licensed}
      <div class="gate">
        <Lock size={14} />
        <p>{i18n.t("agentChat.gate.notLicensed")}</p>
        <button class="gate-btn" onclick={() => ui.openSettings("provider")}>
          <SettingsIcon size={13} /> {i18n.t("agentChat.gate.addLicense")}
        </button>
      </div>
    {:else if !covered}
      <div class="gate">
        <Lock size={14} />
        <p>{i18n.t("agentChat.gate.notCovered")}</p>
      </div>
    {:else if !persona}
      <div class="gate">
        <p>{i18n.t("agentChat.gate.loadingPersona")}</p>
      </div>
    {:else}
      <div class="chat-meta">
        <span>{meterLabel}</span>
        <span class="chat-remaining">
          {i18n.t("agentChat.remaining")}: {remaining}
        </span>
        {#if turns.length > 0}
          <button class="chat-clear" onclick={clear} title={i18n.t("agentChat.clear")}>
            <Trash2 size={13} />
          </button>
        {/if}
      </div>

      <div class="chat-turns">
        {#if turns.length === 0}
          <p class="chat-empty">{i18n.t("agentChat.empty")}</p>
        {/if}
        {#each turns as turn, index (index)}
          <div class="turn" data-role={turn.role}>
            <div class="bubble">
              {#if turn.role === "assistant"}
                <!-- Markdown from our deterministic escaping renderer
                     (util/markdown.ts) — the only source of this HTML. -->
                <div class="markdown">{@html renderMarkdown(turn.content)}</div>
              {:else}
                <p>{turn.content}</p>
              {/if}
              {#if turn.tokens != null || turn.seconds != null}
                <small class="usage">
                  {#if turn.tokens != null}
                    {i18n.t("agentChat.usageTokens", { tokens: formatCount(turn.tokens) })}
                  {:else}
                    {i18n.t("agentChat.usageTime", {
                      seconds: formatCount(turn.seconds ?? 0),
                    })}
                  {/if}
                  {#if turn.estimated}· {i18n.t("agentChat.estimated")}{/if}
                </small>
              {/if}
            </div>
          </div>
        {/each}
        {#if busy}
          <div class="turn" data-role="assistant">
            <div class="bubble typing" aria-label={i18n.t("agentChat.thinking")}>
              <i></i><i></i><i></i>
            </div>
          </div>
        {/if}
      </div>

      {#if error}
        <p class="chat-error">{error}</p>
      {/if}

      <div class="chat-compose">
        <textarea
          rows="2"
          value={input}
          placeholder={i18n.t("agentChat.placeholder")}
          oninput={(e) => (input = (e.currentTarget as HTMLTextAreaElement).value)}
          onkeydown={onKeydown}
          disabled={busy}
          aria-label={i18n.t("agentChat.placeholder")}
        ></textarea>
        <button class="send" onclick={() => void send()} disabled={busy || input.trim() === ""}>
          <Send size={14} /> {i18n.t("agentChat.send")}
        </button>
      </div>
      <p class="chat-foot">{i18n.t("agentChat.footnote")}</p>
    {/if}
  {/if}
</section>

<style>
  .chat {
    border-top: 1px solid var(--color-border);
    padding: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .chat-toggle {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    background: transparent;
    border: 0;
    padding: 0;
    cursor: pointer;
    color: var(--color-text-primary);
    font-size: var(--text-body-sm);
    font-weight: var(--fw-semibold);
    text-align: start;
  }
  .chat-toggle:hover { color: var(--color-brand); }
  .chat-toggle-label { flex: none; }
  .chat-toggle-hint {
    margin-inline-start: auto;
    font-weight: var(--fw-regular, 400);
    font-size: var(--text-caption);
    color: var(--color-text-muted);
    max-width: 60%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gate {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-surface-sunken);
    padding: var(--space-3);
    font-size: var(--text-caption);
    color: var(--color-text-secondary);
  }
  .gate p { margin: 0; flex: 1 1 14rem; }
  .gate-btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 26px;
    padding: 0 10px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--color-border);
    background: var(--color-surface-raised);
    color: var(--color-text-secondary);
    font-size: var(--text-caption);
    cursor: pointer;
  }
  .gate-btn:hover { color: var(--color-text-primary); border-color: var(--color-brand); }
  .chat-meta {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    font-size: var(--text-caption);
    color: var(--color-text-muted);
  }
  .chat-remaining { margin-inline-start: auto; }
  .chat-clear {
    background: transparent;
    border: 0;
    color: var(--color-text-muted);
    cursor: pointer;
    display: inline-flex;
  }
  .chat-clear:hover { color: var(--color-danger, #ef4444); }
  .chat-turns {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    max-height: 22rem;
    overflow-y: auto;
    padding-inline-end: 2px;
  }
  .chat-empty { margin: 0; font-size: var(--text-caption); color: var(--color-text-muted); }
  .turn { display: flex; }
  .turn[data-role="user"] { justify-content: flex-end; }
  .bubble {
    max-width: 92%;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-surface-sunken);
    padding: var(--space-2) var(--space-3);
    font-size: var(--text-body-sm);
    line-height: var(--lh-normal, 1.5);
    color: var(--color-text-primary);
    overflow-wrap: anywhere;
  }
  .turn[data-role="user"] .bubble {
    background: var(--color-brand-subtle);
    border-color: var(--color-brand);
  }
  .bubble p { margin: 0; }
  .usage { display: block; margin-top: 4px; color: var(--color-text-muted); }
  .typing { display: inline-flex; gap: 4px; }
  .typing i {
    width: 5px;
    height: 5px;
    border-radius: var(--radius-full);
    background: var(--color-text-muted);
    animation: chat-blink 1.2s infinite ease-in-out;
  }
  .typing i:nth-child(2) { animation-delay: 0.15s; }
  .typing i:nth-child(3) { animation-delay: 0.3s; }
  @keyframes chat-blink {
    0%, 80%, 100% { opacity: 0.25; }
    40% { opacity: 1; }
  }
  .chat-error { margin: 0; font-size: var(--text-caption); color: var(--color-danger, #ef4444); }
  .chat-compose { display: flex; gap: var(--space-2); align-items: flex-end; }
  .chat-compose textarea {
    flex: 1;
    resize: none;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-surface-sunken);
    color: var(--color-text-primary);
    font: inherit;
    font-size: var(--text-body-sm);
    padding: var(--space-2) var(--space-3);
  }
  .chat-compose textarea:focus-visible { outline: 2px solid var(--color-brand); outline-offset: 1px; }
  .send {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 34px;
    padding: 0 12px;
    border-radius: var(--radius-md);
    border: 1px solid var(--color-brand);
    background: var(--color-brand);
    color: var(--color-text-inverse);
    font-size: var(--text-caption);
    font-weight: var(--fw-semibold);
    cursor: pointer;
  }
  .send:disabled { opacity: 0.5; cursor: default; }
  .chat-foot { margin: 0; font-size: var(--text-caption); color: var(--color-text-muted); }
</style>
