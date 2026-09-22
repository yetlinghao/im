// The only module that talks to the host. In Tauri it forwards to the Rust
// commands; in a plain browser (vite dev, screenshots) it runs an in-memory
// mock so the whole UI can be exercised without the native shell.

import { contentWith, textOf } from "./content";
import { type ImageSource, isImagePath } from "./images";
import { isWindows } from "./platform";
import type {
  ContextItem,
  Message,
  Protocol,
  ProviderInput,
  ProviderProbeResult,
  ProviderView,
  Session,
  SessionSummary,
  Settings,
  TurnEvent,
  TurnKind,
} from "./types";

export interface Backend {
  listSessions(): Promise<SessionSummary[]>;
  getSession(id: string): Promise<Session>;
  deleteSession(id: string): Promise<void>;
  renameSession(id: string, title: string): Promise<Session>;
  setSessionModel(
    id: string,
    providerId: string,
    model: string,
  ): Promise<Session>;
  runTurn(kind: TurnKind, onEvent: (e: TurnEvent) => void): Promise<void>;
  cancelTurn(sessionId: string): Promise<boolean>;
  activeTurns(ids: string[]): Promise<string[]>;

  getProviders(): Promise<ProviderView[]>;
  saveProvider(input: ProviderInput): Promise<ProviderView[]>;
  deleteProvider(id: string): Promise<ProviderView[]>;
  fetchModels(
    protocol: Protocol,
    baseUrl: string,
    apiKey?: string,
    providerId?: string,
  ): Promise<string[]>;
  probeProvider(
    protocol: Protocol,
    baseUrl: string,
    apiKey?: string,
    providerId?: string,
    model?: string,
  ): Promise<ProviderProbeResult>;

  getSettings(): Promise<Settings>;
  saveSettings(settings: Settings): Promise<void>;
  dataDir(): Promise<string>;
  revealDataDir(): Promise<void>;
  exportJsonl(path: string): Promise<number>;
  exportSession(id: string, path: string): Promise<void>;

  popupMenu(items: ContextItem[]): Promise<void>;
  onMenu(cb: (id: string) => void): Promise<() => void>;
  /** Text handed over by the quick-input panel (Return there): open a new chat with it. */
  onQuick(cb: (text: string) => void): Promise<() => void>;
  /** Whether the panel may read the selection in other apps (macOS Accessibility access). */
  quickAccess(): Promise<boolean>;
  /** Ask for that access; the OS shows its own prompt. */
  requestQuickAccess(): Promise<boolean>;
  /** Native file chooser, images only; the chosen files' bytes. */
  pickImages(): Promise<ImageSource[]>;
  /** Image files dragged onto the window; `hover` tracks whether one is over it right now. */
  onDrop(
    cb: (files: ImageSource[]) => void,
    hover: (over: boolean) => void,
  ): Promise<() => void>;
  copyText(text: string): Promise<void>;
  openUrl(url: string): Promise<void>;
  saveDialog(defaultName: string, ext: string): Promise<string | null>;
  confirm(message: string, title: string, okLabel: string): Promise<boolean>;
  showWindow(): Promise<void>;
  /** This build's version (tauri.conf.json). */
  version(): Promise<string>;
  /** Ask the release feed; null when this build is current. */
  checkUpdate(): Promise<{ version: string; notes?: string } | null>;
  /** Download + install the update found by `checkUpdate`, then relaunch. */
  installUpdate(onProgress: (fraction: number) => void): Promise<void>;
  /** Debug-only scenario hooks (env in Tauri, `?state=` in the browser). `attach` is a file path to read as a drop would. */
  scenario(): Promise<{
    state?: string | null;
    autosend?: string | null;
    attach?: string | null;
    query?: string | null;
  } | null>;
}

export const isTauri = "__TAURI_INTERNALS__" in window;

async function tauriBackend(): Promise<Backend> {
  const { invoke, Channel } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  const { save, ask, open } = await import("@tauri-apps/plugin-dialog");
  const { getVersion } = await import("@tauri-apps/api/app");
  let pendingUpdate: import("@tauri-apps/plugin-updater").Update | null = null;
  // Paths come from the OS (drop, dialog); the bytes come back as an ArrayBuffer.
  const readImages = (paths: string[]) =>
    Promise.all(
      paths.filter(isImagePath).map((path) =>
        invoke<ArrayBuffer>("read_image", { path }).catch((e) => {
          console.warn(`read_image ${path}:`, e);
          return null;
        }),
      ),
    ).then((r) => r.filter((b): b is ArrayBuffer => b !== null));

  return {
    listSessions: () => invoke("list_sessions"),
    getSession: (id) => invoke("get_session", { id }),
    deleteSession: (id) => invoke("delete_session", { id }),
    renameSession: (id, title) => invoke("rename_session", { id, title }),
    setSessionModel: (id, providerId, model) =>
      invoke("set_session_model", { id, providerId, model }),
    runTurn: (kind, onEvent) => {
      const onEventChannel = new Channel<TurnEvent>();
      onEventChannel.onmessage = onEvent;
      return invoke("run_turn", { kind, onEvent: onEventChannel });
    },
    cancelTurn: (sessionId) => invoke("cancel_turn", { sessionId }),
    activeTurns: (ids) => invoke("active_turns", { ids }),

    getProviders: () => invoke("get_providers"),
    saveProvider: (input) => invoke("save_provider", { input }),
    deleteProvider: (id) => invoke("delete_provider", { id }),
    fetchModels: (protocol, baseUrl, apiKey, providerId) =>
      invoke("fetch_models", {
        protocol,
        baseUrl,
        apiKey: apiKey ?? null,
        providerId: providerId ?? null,
      }),

    probeProvider: (protocol, baseUrl, apiKey, providerId, model) =>
      invoke("probe_provider", {
        protocol,
        baseUrl,
        apiKey: apiKey ?? null,
        providerId: providerId ?? null,
        model: model ?? null,
      }),
    getSettings: () => invoke("get_settings"),
    saveSettings: (settings) => invoke("save_settings", { settings }),
    dataDir: () => invoke("data_dir"),
    revealDataDir: () => invoke("reveal_data_dir"),
    exportJsonl: (path) => invoke("export_jsonl", { path }),
    exportSession: (id, path) => invoke("export_session", { id, path }),

    popupMenu: (items) => invoke("popup_menu", { items }),
    onMenu: (cb) => listen<string>("menu", (e) => cb(e.payload)),
    onQuick: (cb) => listen<string>("quick:send", (e) => cb(e.payload)),
    quickAccess: () => invoke("quick_access"),
    requestQuickAccess: () => invoke("quick_request_access"),
    pickImages: async () => {
      const picked = await open({
        multiple: true,
        filters: [
          {
            name: "Images",
            extensions: [
              "png",
              "jpg",
              "jpeg",
              "gif",
              "webp",
              "heic",
              "heif",
              "avif",
              "bmp",
              "tiff",
              "tif",
            ],
          },
        ],
      });
      return readImages(
        Array.isArray(picked) ? picked : picked ? [picked] : [],
      );
    },
    onDrop: (cb, hover) =>
      getCurrentWebview().onDragDropEvent(async (e) => {
        const p = e.payload;
        if (p.type === "enter") hover(p.paths.some(isImagePath));
        else if (p.type === "leave") hover(false);
        else if (p.type === "drop") {
          hover(false);
          const files = await readImages(p.paths);
          if (files.length) cb(files);
        }
      }),
    copyText: (text) => writeText(text),
    openUrl: (url) => openUrl(url),
    saveDialog: async (defaultName, ext) =>
      save({
        defaultPath: defaultName,
        filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
      }),
    confirm: (message, title, okLabel) =>
      ask(message, { title, kind: "warning", okLabel, cancelLabel: "Cancel" }),
    showWindow: async () => {
      const w = getCurrentWindow();
      await w.show();
      await w.setFocus();
    },
    version: () => getVersion(),
    checkUpdate: async () => {
      const { check } = await import("@tauri-apps/plugin-updater");
      pendingUpdate = await check();
      return pendingUpdate
        ? {
            version: pendingUpdate.version,
            notes: pendingUpdate.body ?? undefined,
          }
        : null;
    },
    installUpdate: async (onProgress) => {
      if (!pendingUpdate) throw new Error("no update to install");
      let total = 0;
      let got = 0;
      await pendingUpdate.downloadAndInstall((ev) => {
        if (ev.event === "Started") total = ev.data.contentLength ?? 0;
        else if (ev.event === "Progress") {
          got += ev.data.chunkLength;
          if (total) onProgress(Math.min(1, got / total));
        } else if (ev.event === "Finished") onProgress(1);
      });
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    },
    scenario: () => invoke("debug_scenario"),
  };
}

// ---- mock -------------------------------------------------------------------

const SAMPLE_REPLY = `Here's the short version.

**Streaming** arrives token by token; the UI coalesces deltas to one paint per frame, so cost stays proportional to the *last* message rather than the whole transcript.

\`\`\`rust
pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
    self.buf.extend_from_slice(chunk);
    // split on blank lines, not on newlines
}
\`\`\`

A few things worth remembering:

1. Sessions are plain JSON — \`messages[]\` is a replayable \`{role, content}\` list.
2. Per-turn metadata (model, tokens, latency) lives on the assistant message.
3. Keys live in a separate file, so exports never leak them.

> The system prompt is copied into the session, so a trajectory is self-contained.

| protocol | endpoint |
|---|---|
| chat | \`/chat/completions\` |
| anthropic | \`/messages\` |
| responses | \`/responses\` |

That's it — see [the README](https://example.com) for the schema.`;

const SAMPLE_HTML = `Here's a minimal one:

\`\`\`html
<!doctype html>
<html>
  <head>
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <style>
      body { margin: 0; font: 18px/1.5 -apple-system, sans-serif; color: #1d1d1f; background: linear-gradient(160deg, #fdfbfb, #ebedee); }
      main { max-width: 640px; margin: 12vh auto; padding: 0 24px; }
      h1 { font-size: 56px; letter-spacing: -0.03em; margin: 0 0 12px; }
      p { color: #6e6e73; margin: 0 0 28px; }
      a { display: inline-block; padding: 12px 22px; border-radius: 999px; background: #0071e3; color: #fff; text-decoration: none; }
    </style>
  </head>
  <body>
    <main>
      <h1>just chat.</h1>
      <p>An ultra-lightweight chat client. Your providers, your models, plain JSON files.</p>
      <a href="#">Download</a>
    </main>
  </body>
</html>
\`\`\`

Drop it in a file and open it.`;

const SAMPLE_SVG = `A sleeping cat, roughly:

\`\`\`svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 120" width="400" height="240">
  <path d="M40 70 q30 -40 60 -10 q30 -30 60 10 q-10 30 -60 30 q-50 0 -60 -30z" fill="none" stroke="#1d1d1f" stroke-width="6" stroke-linejoin="round"/>
  <path d="M70 62 q10 6 20 0 M110 62 q10 6 20 0" fill="none" stroke="#1d1d1f" stroke-width="4" stroke-linecap="round"/>
  <circle cx="100" cy="76" r="3" fill="#1d1d1f"/>
</svg>
\`\`\`

Save it as cat.svg.`;

/** A "screenshot" drawn on a canvas, for the image scenarios (`?state=image|attach`). */
export function sampleImage(): string {
  const c = document.createElement("canvas");
  c.width = 960;
  c.height = 600;
  const ctx = c.getContext("2d");
  if (!ctx) throw new Error("Could not create the sample image canvas");
  const bg = ctx.createLinearGradient(0, 0, 960, 600);
  bg.addColorStop(0, "#dfe7f3");
  bg.addColorStop(1, "#f4e9dc");
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, 960, 600);
  ctx.fillStyle = "#fff";
  ctx.beginPath();
  ctx.roundRect(120, 90, 720, 420, 14);
  ctx.fill();
  ctx.fillStyle = "#1d1d1f";
  ctx.font = "600 26px -apple-system, Helvetica, sans-serif";
  ctx.fillText("Weekly active users", 160, 145);
  const bars = [0.35, 0.5, 0.42, 0.66, 0.74, 0.9, 0.82];
  bars.forEach((v, i) => {
    ctx.fillStyle = i === 5 ? "#0071e3" : "#c9d6ea";
    const hgt = v * 280;
    ctx.beginPath();
    ctx.roundRect(170 + i * 92, 470 - hgt, 60, hgt, 6);
    ctx.fill();
  });
  return c.toDataURL("image/png");
}

function mockBackend(): Backend {
  const params = new URLSearchParams(location.search);
  const state = params.get("state") ?? "chat";
  const now = () => new Date().toISOString();
  let menuCb: ((id: string) => void) | null = null;
  const cancels = new Map<string, () => void>();

  let settings: Settings = {
    schema_version: 1,
    appearance: (params.get("theme") as Settings["appearance"]) ?? "system",
    max_tokens: 8192,
    sidebar_visible: state !== "nosidebar",
    inspector_visible: state === "json" || params.get("inspector") === "1",
    default_provider_id: "openrouter",
    default_model: "anthropic/claude-sonnet-4",
    quick_shortcut: isWindows ? "" : "Alt+Space",
  };
  let quickAccess = params.has("access");
  let providers: ProviderView[] =
    state === "noproviders"
      ? []
      : [
          {
            id: "openrouter",
            name: "OpenRouter",
            protocol: "chat",
            base_url: "https://openrouter.ai/api/v1",
            models: [
              "anthropic/claude-sonnet-4",
              "openai/gpt-5",
              "deepseek/deepseek-r1",
              "google/gemini-2.5-pro",
            ],
            has_key: true,
          },
          {
            id: "anthropic",
            name: "Anthropic",
            protocol: "anthropic",
            base_url: "https://api.anthropic.com/v1",
            models: ["claude-sonnet-4-5", "claude-opus-4-1"],
            has_key: false,
          },
          {
            id: "openai",
            name: "OpenAI",
            protocol: "responses",
            base_url: "https://api.openai.com/v1",
            models: ["gpt-5", "gpt-5-mini"],
            has_key: true,
          },
        ];

  const meta = (model: string, out: number): NonNullable<Message["meta"]> => ({
    provider_id: "openrouter",
    protocol: "chat",
    model,
    created_at: now(),
    latency_ms: 1840,
    ttft_ms: 412,
    usage: {
      input_tokens: 1283,
      cached_input_tokens: 1024,
      output_tokens: out,
    },
    finish_reason: "stop",
  });

  const sessions = new Map<string, Session>();
  const getSession = (id: string): Session => {
    const session = sessions.get(id);
    if (!session) throw new Error(`session not found: ${id}`);
    return session;
  };
  const mk = (
    id: string,
    title: string,
    ago: number,
    messages: Message[],
  ): Session => ({
    schema_version: 1,
    id,
    title,
    created_at: new Date(Date.now() - ago).toISOString(),
    updated_at: new Date(Date.now() - ago).toISOString(),
    provider_id: "openrouter",
    model: "anthropic/claude-sonnet-4",
    messages,
  });
  if (state !== "empty" && state !== "noproviders") {
    sessions.set(
      "s1",
      mk("s1", "How does the streaming pipeline work?", 60_000, [
        {
          role: "user",
          content:
            "How does the streaming pipeline work? Keep it short, with a code sample.",
          created_at: now(),
        },
        {
          role: "assistant",
          content: SAMPLE_REPLY,
          created_at: now(),
          reasoning_content:
            "The user wants a compact explanation. I should lead with the per-frame coalescing since that's the interesting part, then the schema.",
          meta: meta("anthropic/claude-sonnet-4", 236),
        },
        {
          role: "user",
          content: "And what happens on cancel?",
          created_at: now(),
        },
        {
          role: "assistant",
          content:
            'The stream is dropped, and whatever text already arrived is persisted with `finish_reason: "cancelled"` — so a truncated reply is still a faithful trajectory.',
          created_at: now(),
          meta: meta("anthropic/claude-sonnet-4", 41),
        },
      ]),
    );
    sessions.set(
      "s2",
      mk("s2", "Rust lifetimes in async closures", 3_600_000, [
        {
          role: "user",
          content: "Rust lifetimes in async closures",
          created_at: now(),
        },
      ]),
    );
    sessions.set(
      "s3",
      mk("s3", "Draft: release notes for 0.1", 86_400_000, [
        {
          role: "user",
          content: "Draft: release notes for 0.1",
          created_at: now(),
        },
      ]),
    );
    sessions.set(
      "s4",
      mk(
        "s4",
        "Why does TextKit grow blocks with paragraphSpacing",
        3 * 86_400_000,
        [{ role: "user", content: "Why?", created_at: now() }],
      ),
    );
    if (state === "error") {
      const s = getSession("s1");
      s.messages.push({
        role: "user",
        content: "One more thing…",
        created_at: now(),
      });
    }
    if (
      state === "html" ||
      state === "html-expanded" ||
      state === "select-test"
    ) {
      const s = getSession("s1");
      s.messages.push({
        role: "user",
        content: "Make me a tiny landing page.",
        created_at: now(),
      });
      s.messages.push({
        role: "assistant",
        content: SAMPLE_HTML,
        created_at: now(),
        meta: meta("anthropic/claude-sonnet-4", 310),
      });
    }
    if (state === "svg") {
      const s = getSession("s1");
      s.messages.push({
        role: "user",
        content: "Draw the cat as an SVG.",
        created_at: now(),
      });
      s.messages.push({
        role: "assistant",
        content: SAMPLE_SVG,
        created_at: now(),
        meta: meta("anthropic/claude-sonnet-4", 120),
      });
    }
    if (state === "image" || state === "image-expanded") {
      const s = getSession("s1");
      s.messages.push({
        role: "user",
        content: contentWith("What does this chart say?", [sampleImage()]),
        created_at: now(),
      });
      s.messages.push({
        role: "assistant",
        content:
          "Weekly active users climbed for most of the period — from roughly a third of the axis to about 90% at the highlighted bar — then eased slightly in the final week. The blue bar marks the peak.",
        created_at: now(),
        meta: {
          ...meta("anthropic/claude-sonnet-4", 52),
          usage: {
            input_tokens: 1640,
            cached_input_tokens: 0,
            output_tokens: 52,
          },
        },
      });
    }
  }

  const summaries = () =>
    [...sessions.values()]
      .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
      .map((s) => ({
        id: s.id,
        title: s.title,
        updated_at: s.updated_at,
        provider_id: s.provider_id,
        model: s.model,
        message_count: s.messages.length,
      }));

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  return {
    listSessions: async () => summaries(),
    getSession: async (id) => structuredClone(getSession(id)),
    deleteSession: async (id) => {
      sessions.delete(id);
    },
    renameSession: async (id, title) => {
      const s = getSession(id);
      s.title = title;
      return structuredClone(s);
    },
    setSessionModel: async (id, providerId, model) => {
      const s = getSession(id);
      s.provider_id = providerId;
      s.model = model;
      return structuredClone(s);
    },
    runTurn: async (kind, onEvent) => {
      let s: Session;
      if (kind.kind === "send") {
        s = kind.session_id
          ? getSession(kind.session_id)
          : mk(`s${Date.now()}`, "New chat", 0, []);
        s.provider_id = kind.provider_id;
        s.model = kind.model;
        if (s.messages.at(-1)?.role === "user") s.messages.pop();
        s.messages.push({
          role: "user",
          content: contentWith(kind.content, kind.images ?? []),
          created_at: now(),
        });
        if (s.messages.length === 1)
          s.title =
            kind.content.split("\n")[0]?.slice(0, 60) ||
            (kind.images?.length ? "Image" : "New chat");
        sessions.set(s.id, s);
      } else {
        s = getSession(kind.session_id);
        while (s.messages.at(-1)?.role === "assistant") s.messages.pop();
        if (kind.kind === "edit") {
          const last = s.messages.at(-1);
          const content = contentWith(kind.content, kind.images ?? []);
          if (last && last.role === "user") last.content = content;
          else s.messages.push({ role: "user", content, created_at: now() });
        }
      }
      s.updated_at = now();
      onEvent({ type: "started", session: structuredClone(s) });

      if (
        state === "error" ||
        textOf(s.messages.at(-1)?.content ?? "").includes("fail")
      ) {
        await sleep(300);
        onEvent({
          type: "done",
          session_id: s.id,
          error: "HTTP 401: Invalid API key",
          updated_at: now(),
        });
        return;
      }

      let cancelled = false;
      cancels.set(s.id, () => (cancelled = true));
      const reasoning =
        "The user is asking a follow-up. I'll answer briefly and reuse the earlier framing.";
      const reply =
        state === "streaming"
          ? SAMPLE_REPLY
          : state === "streaming-html"
            ? SAMPLE_HTML
            : `Sure — ${SAMPLE_REPLY.slice(0, 400)}`;
      const holdAt =
        state === "streaming"
          ? 420
          : state === "streaming-html"
            ? 700
            : Infinity;
      let text = "";
      let think = "";
      await sleep(250);
      for (const ch of reasoning.match(/.{1,6}/gs) ?? []) {
        if (cancelled) break;
        think += ch;
        onEvent({ type: "reasoning", session_id: s.id, delta: ch });
        await sleep(18);
      }
      for (const ch of reply.match(/.{1,5}/gs) ?? []) {
        if (cancelled) break;
        text += ch;
        onEvent({ type: "text", session_id: s.id, delta: ch });
        await sleep(holdAt < Infinity ? 40 : 12);
        if (text.length > holdAt) {
          // Hold mid-stream so screenshots catch the live state.
          await new Promise<void>((r) =>
            cancels.set(s.id, () => {
              cancelled = true;
              r();
            }),
          );
          break;
        }
      }
      cancels.delete(s.id);
      const message: Message = {
        role: "assistant",
        content: text,
        reasoning_content: think,
        created_at: now(),
        meta: {
          ...meta(s.model, 120),
          finish_reason: cancelled ? "cancelled" : "stop",
        },
      };
      s.messages.push(message);
      s.updated_at = now();
      onEvent({
        type: "done",
        session_id: s.id,
        message,
        updated_at: s.updated_at,
      });
    },
    cancelTurn: async (id) => {
      const c = cancels.get(id);
      c?.();
      return !!c;
    },
    activeTurns: async (ids) => ids.filter((id) => cancels.has(id)),

    getProviders: async () => structuredClone(providers),
    saveProvider: async (input) => {
      const { api_key, ...p } = input;
      const existing = providers.find((x) => x.id === p.id);
      const view: ProviderView = {
        ...p,
        has_key:
          api_key !== undefined ? api_key !== "" : (existing?.has_key ?? false),
      };
      providers = existing
        ? providers.map((x) => (x.id === p.id ? view : x))
        : [...providers, view];
      return structuredClone(providers);
    },
    deleteProvider: async (id) => {
      providers = providers.filter((p) => p.id !== id);
      return structuredClone(providers);
    },
    fetchModels: async (protocol) => {
      await sleep(400);
      return protocol === "anthropic"
        ? ["claude-sonnet-4-5", "claude-opus-4-1", "claude-haiku-4-5"]
        : ["gpt-5", "gpt-5-mini", "o4-mini"];
    },

    probeProvider: async (protocol, _baseUrl, apiKey, providerId, model) => {
      const mode = params.get("probe");
      const duration = mode === "timeout" ? 1200 : 450;
      await sleep(duration);
      const status =
        mode === "auth"
          ? 401
          : mode === "404"
            ? 404
            : mode === "rate"
              ? 429
              : mode === "unexpected"
                ? 200
                : mode === "long" || mode === "error"
                  ? 500
                  : null;
      const errorCategory: ProviderProbeResult["error_category"] =
        mode === "auth"
          ? "authentication"
          : mode === "404"
            ? "endpoint"
            : mode === "rate"
              ? "rate_limited"
              : mode === "timeout"
                ? "timeout"
                : mode === "unexpected"
                  ? "unexpected_response"
                  : mode === "long" || mode === "error"
                    ? "provider"
                    : null;
      const fail = errorCategory !== null;
      const message =
        mode === "auth"
          ? "The API key is invalid or lacks permission."
          : mode === "404"
            ? "The endpoint path was not found."
            : mode === "rate"
              ? "The provider is rate limited or out of quota."
              : mode === "timeout"
                ? "The provider request timed out."
                : mode === "unexpected"
                  ? "The provider returned an unexpected response for this protocol."
                  : mode === "key"
                    ? apiKey?.trim()
                      ? "Temporary API key sent."
                      : providerId
                        ? "Saved API key selected."
                        : "No API key supplied."
                    : mode === "long"
                      ? "The provider rejected the request. " +
                        "Diagnostic detail. ".repeat(12)
                      : "The provider rejected the request.";
      const listed =
        protocol === "anthropic"
          ? ["claude-sonnet-4-5", "claude-opus-4-1"]
          : ["gpt-5", "gpt-5-mini"];
      return {
        ok: !fail,
        phase: model ? "stream" : "models",
        status,
        duration_ms: duration + 2,
        model_count: fail ? null : listed.length,
        stream_ok: !fail && !!model,
        error_category: errorCategory,
        message: fail
          ? message
          : mode === "key"
            ? message
            : model
              ? "Streaming test succeeded."
              : `Connection successful; ${listed.length} models found.`,
        detail:
          mode === "timeout"
            ? "Request timed out"
            : status === null
              ? null
              : `HTTP ${status}`,
        models_warning: null,
        models: fail ? null : listed,
      };
    },
    getSettings: async () => ({ ...settings }),
    saveSettings: async (s) => {
      settings = { ...s };
    },
    dataDir: async () => "~/Library/Application Support/im",
    revealDataDir: async () => {},
    exportJsonl: async () => sessions.size,
    exportSession: async () => {},

    popupMenu: async (items) => {
      const label = items
        .filter((i) => !i.separator && i.enabled !== false)
        .map((i) => i.label)
        .join(" / ");
      const pick = window.prompt(
        `Context menu:\n${label}\n\nType an item id`,
        items.find((i) => i.id)?.id ?? "",
      );
      if (pick) menuCb?.(pick);
    },
    onMenu: async (cb) => {
      menuCb = cb;
      return () => (menuCb = null);
    },
    // `?quick=<text>` plays the panel's hand-over shortly after launch.
    onQuick: async (cb) => {
      const text = params.get("quick");
      if (text) setTimeout(() => cb(text), 300);
      return () => {};
    },
    quickAccess: async () => quickAccess,
    requestQuickAccess: async () => (quickAccess = true),
    pickImages: () =>
      new Promise((resolve) => {
        const input = document.createElement("input");
        input.type = "file";
        input.multiple = true;
        input.accept = "image/*";
        input.onchange = () => resolve(Array.from(input.files ?? []));
        input.oncancel = () => resolve([]);
        input.click();
      }),
    onDrop: async (cb, hover) => {
      const hasFiles = (e: DragEvent) =>
        Array.from(e.dataTransfer?.types ?? []).includes("Files");
      const over = (e: DragEvent) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        hover(true);
      };
      const leave = (e: DragEvent) => {
        if (e.relatedTarget === null) hover(false);
      };
      const drop = (e: DragEvent) => {
        if (!hasFiles(e)) return;
        e.preventDefault();
        hover(false);
        const files = Array.from(e.dataTransfer?.files ?? []).filter(
          (f) => f.type.startsWith("image/") || isImagePath(f.name),
        );
        if (files.length) cb(files);
      };
      window.addEventListener("dragenter", over);
      window.addEventListener("dragover", over);
      window.addEventListener("dragleave", leave);
      window.addEventListener("drop", drop);
      return () => {
        window.removeEventListener("dragenter", over);
        window.removeEventListener("dragover", over);
        window.removeEventListener("dragleave", leave);
        window.removeEventListener("drop", drop);
      };
    },
    copyText: (text) => navigator.clipboard.writeText(text),
    openUrl: async (url) => {
      window.open(url, "_blank");
    },
    saveDialog: async (name) => window.prompt("Save as", name),
    confirm: async (message) => window.confirm(message),
    showWindow: async () => {},
    version: async () => "0.1.0",
    // `?update=1` pretends the feed has 0.2.0; installing "downloads" for a second and stops.
    checkUpdate: async () =>
      params.has("update")
        ? {
            version: "0.2.0",
            notes: "Live HTML preview, line numbers, trajectory column.",
          }
        : null,
    installUpdate: async (onProgress) => {
      for (let i = 1; i <= 10; i++) {
        await sleep(100);
        onProgress(i / 10);
      }
    },
    scenario: async () => ({ state, autosend: params.get("autosend") }),
  };
}

export async function createBackend(): Promise<Backend> {
  return isTauri ? tauriBackend() : mockBackend();
}
