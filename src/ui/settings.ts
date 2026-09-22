// In-window settings, laid out like System Settings: grouped lists with a
// label on the left and the control on the right. Every field saves itself
// when you leave it — there is no Save button. "Add Provider" opens an empty
// card; it becomes a real provider the moment it has a name.

import * as actions from "../actions";
import type { Backend } from "../api";
import { h, type IconName, icon, replaceChildren } from "../dom";
import { isMacOS, isWindows } from "../platform";
import { PROTOCOLS } from "../presets";
import { prettyShortcut, shortcutFromEvent } from "../shortcut";
import { type State, store } from "../state";
import type { Appearance, Protocol, ProviderView } from "../types";
import {
  isCurrentProviderProbe,
  normalizeFetchedModels,
  resolveProbeCredentials,
  shouldClearApiKeySnapshot,
} from "./provider-probe-state";

export function createSettings(backend: Backend): HTMLElement {
  const providersList = h("div", { class: "groups" });
  const general = h("div", { class: "group" });
  const data = h("div", { class: "group" });
  const addProvider = h(
    "button",
    {
      class: "icon-btn",
      type: "button",
      title: "Add Provider",
      "aria-label": "Add Provider",
      onclick: () => addDraft(),
    },
    icon("plus"),
  );

  const root = h(
    "div",
    { class: "settings", hidden: true },
    h(
      "div",
      { class: "settings-scroll" },
      h(
        "div",
        { class: "settings-column" },
        h("h1", null, "Settings"),
        section("Providers", addProvider),
        providersList,
        section("General"),
        general,
        section("Data"),
        data,
      ),
    ),
  );

  // Cards are keyed by provider id and updated in place, so saving one field
  // never rebuilds (and un-focuses) the card being typed in. A draft card has
  // no id yet; it registers itself once the first save gives it one.
  const cards = new Map<string, ProviderCard>();
  let draft: ProviderCard | null = null;
  const renderProviders = (providers: ProviderView[]) => {
    const seen = new Set<string>();
    const els: HTMLElement[] = [];
    for (const p of providers) {
      seen.add(p.id);
      let card = cards.get(p.id);
      if (!card) {
        card = providerCard(p, backend, hooks);
        cards.set(p.id, card);
      } else {
        card.update(p);
      }
      els.push(card.el);
    }
    for (const id of [...cards.keys()]) if (!seen.has(id)) cards.delete(id);
    if (draft) els.push(draft.el);
    replaceChildren(
      providersList,
      els.length
        ? els
        : [h("div", { class: "group-empty" }, "No providers yet.")],
    );
  };
  const hooks: CardHooks = {
    created(id, card) {
      cards.set(id, card);
      if (draft === card) draft = null;
    },
    discard(card) {
      if (draft === card) {
        draft = null;
        card.el.remove();
        if (cards.size === 0) renderProviders(store.state.providers);
      }
    },
  };
  const addDraft = () => {
    if (draft) {
      draft.focus();
      return;
    }
    draft = providerCard(
      {
        id: "",
        name: "",
        protocol: "chat",
        base_url: "",
        models: [],
        has_key: false,
      },
      backend,
      hooks,
    );
    providersList.querySelector(".group-empty")?.remove();
    providersList.append(draft.el);
    draft.focus();
  };

  // "Version 0.1.0 · Check for Updates" → "0.2.0 available · Update" → "Downloading… 42%".
  const versionRow = () => {
    const s = store.state;
    const u = s.update;
    let control: HTMLElement;
    if (u && (u.phase === "downloading" || u.phase === "installing")) {
      control = h(
        "span",
        { class: "srow-value" },
        u.phase === "installing"
          ? "Installing…"
          : `Downloading… ${Math.round((u.progress ?? 0) * 100)}%`,
      );
    } else if (u) {
      control = h(
        "div",
        { class: "srow-inline" },
        h(
          "span",
          {
            class: `srow-value${u.phase === "failed" ? " err" : ""}`,
            title: u.error ?? u.notes ?? "",
          },
          u.phase === "failed" ? "Update failed" : `${u.version} available`,
        ),
        tbtn(
          u.phase === "failed" ? "Retry" : "Update",
          () => void actions.installUpdate(),
        ),
      );
    } else if (s.updateCheck === "checking") {
      control = h("span", { class: "srow-value" }, "Checking…");
    } else if (s.updateCheck === "uptodate") {
      control = h("span", { class: "srow-value" }, "Up to date");
    } else if (s.updateCheck === "failed") {
      control = h(
        "div",
        { class: "srow-inline" },
        h(
          "span",
          { class: "srow-value err" },
          "Couldn't reach the release feed",
        ),
        tbtn("Retry", () => void actions.checkForUpdates(true)),
      );
    } else {
      control = tbtn(
        "Check for Updates",
        () => void actions.checkForUpdates(true),
      );
    }
    return srow("Version", control, s.version ? `im ${s.version}` : undefined);
  };
  let versionEl: HTMLElement | null = null;

  const renderGeneral = (s: State) => {
    const settings = s.settings;
    const appearanceIcons: Record<Appearance, IconName> = {
      system: "display",
      light: "sun",
      dark: "moon",
    };
    const appearanceLabels: Record<Appearance, string> = {
      system: "System",
      light: "Light",
      dark: "Dark",
    };
    const seg = h(
      "div",
      { class: "segmented", role: "radiogroup", "aria-label": "Appearance" },
      (["system", "light", "dark"] as Appearance[]).map((a) =>
        h(
          "button",
          {
            class: `seg icon-seg${settings.appearance === a ? " on" : ""}`,
            role: "radio",
            "aria-checked": String(settings.appearance === a),
            "aria-label": appearanceLabels[a],
            title: appearanceLabels[a],
            onclick: () => void actions.setAppearance(a),
          },
          icon(appearanceIcons[a]),
        ),
      ),
    );
    const prompt = h("textarea", {
      class: "sfield area",
      rows: 3,
      placeholder: "Copied into every new chat as its system prompt.",
      value: s.settings.system_prompt ?? "",
    }) as HTMLTextAreaElement;
    prompt.addEventListener(
      "change",
      () =>
        void actions.saveSettings({
          ...store.state.settings,
          system_prompt: prompt.value.trim() || undefined,
        }),
    );
    versionEl = versionRow();
    replaceChildren(
      general,
      versionEl,
      srow("System prompt", null, undefined, prompt),
      srow("Appearance", seg),
      shortcutRow(),
      ...(isMacOS ? [accessRow(backend)] : []),
    );
  };

  const renderData = async () => {
    replaceChildren(
      data,
      srow(
        "Folder",
        tbtn(isWindows ? "Show in File Explorer" : "Show in Finder", () =>
          actions.revealData(),
        ),
      ),
      srow(
        "Export",
        h(
          "div",
          { class: "srow-inline" },
          h(
            "button",
            {
              class: "tbtn",
              type: "button",
              title: "All chats as JSONL…",
              onclick: () => void actions.exportAll(),
            },
            icon("export"),
          ),
        ),
      ),
    );
  };

  let wasOpen = false;
  let lastProviders: ProviderView[] | null = null;
  store.subscribe((s) => {
    const open = s.view === "settings";
    root.hidden = !open;
    if (open && s.providers !== lastProviders) {
      lastProviders = s.providers;
      renderProviders(s.providers);
    }
    if (open && !wasOpen) {
      renderGeneral(s);
      void renderData();
    } else if (open) {
      // Appearance may have changed from the menu; keep the segmented control honest.
      general.querySelectorAll(".seg").forEach((b, i) => {
        const on =
          (["system", "light", "dark"] as Appearance[])[i] ===
          s.settings.appearance;
        b.classList.toggle("on", on);
        b.setAttribute("aria-checked", String(on));
      });
      if (versionEl) {
        const next = versionRow();
        versionEl.replaceWith(next);
        versionEl = next;
      }
    }
    wasOpen = open;
  });
  return root;
}

function section(title: string, action?: HTMLElement): HTMLElement {
  return h(
    "div",
    { class: `section${action ? " with-action" : ""}` },
    h("h2", null, title),
    action ?? null,
  );
}

/** One list row: label · control (right); optional note under the label and a
 *  full-width block (textarea) beneath. */
function srow(
  label: string,
  control: HTMLElement | null,
  note?: string,
  block?: HTMLElement,
): HTMLElement {
  return h(
    "div",
    { class: `srow${block ? " has-block" : ""}` },
    h(
      "div",
      { class: "srow-label" },
      label,
      note ? h("div", { class: "srow-note" }, note) : null,
    ),
    control ? h("div", { class: "srow-control" }, control) : null,
    block ? h("div", { class: "srow-block" }, block) : null,
  );
}

function tbtn(
  label: string,
  onClick: () => void,
  danger = false,
): HTMLButtonElement {
  return h(
    "button",
    {
      class: `tbtn${danger ? " danger" : ""}`,
      type: "button",
      onclick: onClick,
    },
    label,
  ) as HTMLButtonElement;
}

/** "Quick input · ⌥ Space": click the shortcut, press the new keys; ⌫ turns it off,
 *  Esc keeps the old one. A combination the OS refuses is reported under the label. */
function shortcutRow(): HTMLElement {
  const field = h(
    "button",
    {
      class: "shortcut",
      type: "button",
      title: "Click, then press the new keys. Delete turns it off.",
    },
    prettyShortcut(store.state.settings.quick_shortcut),
  ) as HTMLButtonElement;
  let recording = false;
  const paint = () => {
    field.textContent = recording
      ? "Press keys…"
      : prettyShortcut(store.state.settings.quick_shortcut);
    field.classList.toggle("recording", recording);
  };
  const stop = () => {
    recording = false;
    window.removeEventListener("keydown", onKey, true);
    field.removeEventListener("blur", stop);
    paint();
  };
  const onKey = (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") return stop();
    const accel =
      e.key === "Backspace" || e.key === "Delete" ? "" : shortcutFromEvent(e);
    if (accel === null) return; // a modifier on its own: keep waiting for the key
    stop();
    field.textContent = prettyShortcut(accel);
    void actions.setQuickShortcut(accel).then((err) => {
      paint();
      if (err) {
        field.title = `Couldn't register ${prettyShortcut(accel)} — is another app using it?`;
        console.warn(err);
      }
    });
  };
  field.addEventListener("click", () => {
    if (recording) return stop();
    recording = true;
    paint();
    window.addEventListener("keydown", onKey, true);
    field.addEventListener("blur", stop);
  });
  return h(
    "div",
    { class: "srow" },
    h("div", { class: "srow-label" }, "Quick input"),
    h("div", { class: "srow-control" }, field),
  );
}

/** Reading the selection in other apps needs Accessibility access; the button asks for it. */
function accessRow(backend: Backend): HTMLElement {
  const control = h("div", { class: "srow-inline" });
  let polling = 0;
  const paint = (granted: boolean) => {
    replaceChildren(
      control,
      granted
        ? h("span", { class: "srow-value" }, "Allowed")
        : tbtn("Allow…", () => void request()),
    );
  };
  const check = () => backend.quickAccess().then(paint);
  const request = async () => {
    await backend.requestQuickAccess();
    // The user is off in System Settings; notice when they come back with it on.
    clearInterval(polling);
    let tries = 0;
    polling = window.setInterval(() => {
      void backend.quickAccess().then((ok) => {
        if (ok || ++tries > 90 || store.state.view !== "settings")
          clearInterval(polling);
        if (ok) paint(true);
      });
    }, 1000);
  };
  void check();
  return srow("Quote selection", control);
}

interface ProviderCard {
  el: HTMLElement;
  update(p: ProviderView): void;
  focus(): void;
}

interface CardHooks {
  created(id: string, card: ProviderCard): void;
  discard(card: ProviderCard): void;
}

/** `a-z0-9-` from a name, or the URL's host; unique among the current providers. */
function idFor(name: string, baseUrl: string): string {
  let base = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  if (!base) {
    try {
      base = new URL(baseUrl).hostname.split(".").slice(-2, -1)[0] ?? "";
    } catch {
      base = "";
    }
  }
  base = base || "provider";
  const taken = new Set(store.state.providers.map((p) => p.id));
  let id = base;
  for (let i = 2; taken.has(id); i++) id = `${base}-${i}`;
  return id;
}

function checkedProviderBaseUrl(value: string): string {
  const input = value.trim();
  if (!input) throw new Error("Base URL is required.");
  let url: URL;
  try {
    url = new URL(input);
  } catch {
    throw new Error("Enter a valid URL beginning with http:// or https://.");
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("Base URL must use http:// or https://.");
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new Error(
      "Remove credentials, query parameters, and fragments from the Base URL.",
    );
  }
  const last = url.pathname
    .replace(/\/+$/, "")
    .split("/")
    .at(-1)
    ?.toLowerCase();
  if (["models", "messages", "responses", "completions"].includes(last ?? "")) {
    throw new Error(
      "Enter the API base URL (for example, ending in /v1), not an endpoint path.",
    );
  }
  return url.toString().replace(/\/+$/, "");
}

function providerCard(
  initial: ProviderView,
  backend: Backend,
  hooks: CardHooks,
): ProviderCard {
  let p = initial;
  const isDraft = () => p.id === "";
  const name = h("input", {
    class: "sfield name",
    value: p.name,
    placeholder: "Name",
    spellcheck: false,
  }) as HTMLInputElement;
  const id = h("div", { class: "provider-id" }, p.id);
  const saveStatus = h("span", {
    class: "provider-save-state",
    "aria-live": "polite",
  });
  const protocol = h(
    "select",
    { class: "popup" },
    PROTOCOLS.map((x) =>
      h(
        "option",
        { value: x.value, selected: x.value === p.protocol },
        x.label,
      ),
    ),
  ) as HTMLSelectElement;
  const baseUrl = h("input", {
    class: "sfield mono",
    value: p.base_url,
    placeholder: "https://host/v1",
    spellcheck: false,
    type: "url",
  }) as HTMLInputElement;
  const key = h("input", {
    class: "sfield mono",
    type: "password",
    placeholder: p.has_key ? "••••••••" : "Not set",
    autocomplete: "off",
    spellcheck: false,
  }) as HTMLInputElement;
  const clearKey = tbtn("Clear", () => {
    key.value = "";
    pendingApiKey = { value: "", version: ++apiKeyVersion };
    void save("");
  });
  clearKey.title = "Clear the saved API key";
  clearKey.hidden = !p.has_key;
  const models = h("textarea", {
    class: "sfield area mono",
    rows: rowsFor(p.models.length),
    placeholder: "one model id per line",
    spellcheck: false,
    value: p.models.join("\n"),
  }) as HTMLTextAreaElement;
  const count = h("span", {
    class: "srow-value provider-result",
    "aria-live": "polite",
  });
  const fetchBtn = tbtn("Fetch", () => void fetchModels());
  const testBtn = tbtn("Test", () => void testProvider());
  testBtn.title =
    "Checks the model list and, when a model is configured, sends a one-token streaming request that may incur a small provider charge.";
  const endpoint = h("div", { class: "srow-note mono end" });

  const modelList = () => [
    ...new Set(
      models.value
        .split(/\r?\n/)
        .map((m) => m.trim())
        .filter(Boolean),
    ),
  ];
  const say = (text: string, err = false) => {
    count.textContent = text;
    count.classList.toggle("err", err);
  };
  const paintCount = () =>
    say(
      `${modelList().length || "no"} model${modelList().length === 1 ? "" : "s"}`,
    );
  const paintEndpoint = () => {
    const proto = PROTOCOLS.find((x) => x.value === protocol.value);
    try {
      endpoint.textContent = proto
        ? `${checkedProviderBaseUrl(baseUrl.value)}${proto.path}`
        : "";
    } catch {
      endpoint.textContent = "";
    }
  };
  let saveSequence = 0;
  let saveQueue: Promise<void> = Promise.resolve();
  let feedbackTimer = 0;
  let pendingProviderId: string | null = null;
  let apiKeyVersion = 0;
  let pendingApiKey: { value: string; version: number } | null = null;
  const setSaveStatus = (
    text: string,
    tone: "busy" | "saved" | "error" | "" = "",
  ) => {
    window.clearTimeout(feedbackTimer);
    saveStatus.textContent = text;
    saveStatus.className = `provider-save-state${tone ? ` ${tone}` : ""}`;
    saveStatus.title = tone === "error" ? text : "";
  };

  // A draft becomes real only after both required fields validate. Saves are
  // serialized per card so a slow earlier response cannot overwrite a newer edit.
  const save = (apiKey?: string): Promise<void> => {
    const sequence = ++saveSequence;
    const keyToSave = apiKey ?? pendingApiKey?.value;
    const keyVersion = pendingApiKey?.version;
    if (!name.value.trim()) {
      setSaveStatus("Provider name is required.", "error");
      return Promise.resolve();
    }
    let normalizedUrl: string;
    try {
      normalizedUrl = checkedProviderBaseUrl(baseUrl.value);
    } catch (error) {
      setSaveStatus(
        error instanceof Error ? error.message : "Invalid Base URL.",
        "error",
      );
      return Promise.resolve();
    }
    const creating = isDraft();
    const pid = creating
      ? (pendingProviderId ?? idFor(name.value.trim(), normalizedUrl))
      : p.id;
    if (creating) pendingProviderId = pid;
    const input = {
      id: pid,
      name: name.value.trim(),
      protocol: protocol.value as Protocol,
      base_url: normalizedUrl,
      models: modelList(),
      api_key: keyToSave,
    };
    setSaveStatus("Saving…", "busy");
    const task = saveQueue.then(async () => {
      try {
        const providers = await backend.saveProvider(input);
        if (sequence !== saveSequence) return;
        const saved = providers.find((x) => x.id === pid);
        if (saved) p = saved;
        if (creating && saved) {
          id.textContent = saved.id;
          el.classList.remove("draft");
          footBtn.textContent = "Remove Provider…";
          pendingProviderId = null;
          hooks.created(pid, card);
        }
        if (shouldClearApiKeySnapshot(pendingApiKey, keyVersion)) {
          pendingApiKey = null;
        }
        store.set({ providers });
        if (creating && saved && !store.state.draft) {
          store.set({
            draft: { providerId: saved.id, model: saved.models[0] ?? "" },
          });
        }
        setSaveStatus("Saved", "saved");
        const savedSequence = sequence;
        feedbackTimer = window.setTimeout(() => {
          if (savedSequence === saveSequence) setSaveStatus("");
        }, 1800);
      } catch {
        if (sequence !== saveSequence) return;
        if (creating) pendingProviderId = null;
        setSaveStatus("Save failed. Check the fields and try again.", "error");
        const failedSequence = sequence;
        feedbackTimer = window.setTimeout(() => {
          if (failedSequence === saveSequence) setSaveStatus("");
        }, 2500);
      }
    });
    saveQueue = task.catch(() => undefined);
    return task;
  };

  let probeSequence = 0;
  const isCurrentSettingsCard = () =>
    el.isConnected && store.state.view === "settings";
  const fetchModels = async () => {
    let normalizedUrl: string;
    try {
      normalizedUrl = checkedProviderBaseUrl(baseUrl.value);
    } catch (error) {
      say(error instanceof Error ? error.message : "Invalid Base URL.", true);
      return;
    }
    fetchBtn.disabled = true;
    testBtn.disabled = true;
    say("Fetching…");
    try {
      const ids = await backend.fetchModels(
        protocol.value as Protocol,
        normalizedUrl,
        key.value.trim() || undefined,
        isDraft() ? undefined : p.id,
      );
      if (!isCurrentSettingsCard()) return;
      const received = normalizeFetchedModels(ids);
      if (received.length) {
        models.value = received.join("\n");
        models.rows = rowsFor(received.length);
        await save();
        if (!isCurrentSettingsCard()) return;
        say(`${modelList().length} models found.`);
      } else {
        say("No models were returned.", true);
      }
    } catch {
      if (isCurrentSettingsCard())
        say("Couldn't fetch models. Check the URL, key, and protocol.", true);
    } finally {
      if (isCurrentSettingsCard()) {
        fetchBtn.disabled = false;
        testBtn.disabled = false;
      }
    }
  };

  const testProvider = async () => {
    let normalizedUrl: string;
    try {
      normalizedUrl = checkedProviderBaseUrl(baseUrl.value);
    } catch (error) {
      say(error instanceof Error ? error.message : "Invalid Base URL.", true);
      return;
    }
    const candidates = modelList();
    const configuredModel =
      store.state.draft?.providerId === p.id
        ? store.state.draft.model
        : store.state.settings.default_provider_id === p.id
          ? store.state.settings.default_model
          : undefined;
    const model = candidates.includes(configuredModel ?? "")
      ? configuredModel
      : candidates[0];
    const sequence = ++probeSequence;
    fetchBtn.disabled = true;
    testBtn.disabled = true;
    say(model ? `Testing ${model}…` : "Checking connection…");
    try {
      const credentials = resolveProbeCredentials(
        pendingApiKey,
        key.value,
        p.id,
        isDraft(),
      );
      const result = await backend.probeProvider(
        protocol.value as Protocol,
        normalizedUrl,
        credentials.apiKey,
        credentials.providerId,
        model,
      );
      if (
        !isCurrentProviderProbe(
          sequence,
          probeSequence,
          el.isConnected,
          store.state.view,
        )
      )
        return;
      const metrics = [
        result.phase,
        result.status === null ? null : `HTTP ${result.status}`,
        `${result.duration_ms} ms`,
        result.model_count === null ? null : `${result.model_count} models`,
        result.stream_ok ? "Stream OK" : null,
      ];
      const detail = result.detail?.replace(/^HTTP \d+:?\s*/, "");
      const summary = [
        result.message,
        ...metrics,
        detail,
        result.models_warning,
      ]
        .filter(Boolean)
        .join(" · ");
      say(summary, !result.ok);
    } catch {
      if (
        isCurrentProviderProbe(
          sequence,
          probeSequence,
          el.isConnected,
          store.state.view,
        )
      ) {
        say("Test failed before a result was returned.", true);
      }
    } finally {
      if (
        isCurrentProviderProbe(
          sequence,
          probeSequence,
          el.isConnected,
          store.state.view,
        )
      ) {
        fetchBtn.disabled = false;
        testBtn.disabled = false;
      }
    }
  };

  name.addEventListener("change", () => void save());
  protocol.addEventListener("change", () => {
    paintEndpoint();
    void save();
  });
  baseUrl.addEventListener("input", paintEndpoint);
  baseUrl.addEventListener("change", () => void save());
  key.addEventListener("change", () => {
    const value = key.value.trim();
    if (!value) {
      pendingApiKey = null;
      return;
    }
    pendingApiKey = { value, version: ++apiKeyVersion };
    key.value = "";
    void save(value);
  });
  models.addEventListener("input", paintCount);
  models.addEventListener("change", () => {
    models.rows = rowsFor(modelList().length);
    void save();
  });
  paintEndpoint();
  paintCount();

  const remove = async () => {
    if (isDraft()) {
      hooks.discard(card);
      return;
    }
    const ok = await backend.confirm(
      `Remove “${p.name}”? Its API key is deleted too; chats are kept.`,
      "Remove Provider",
      "Remove",
    );
    if (!ok) return;
    const providers = await backend.deleteProvider(p.id);
    const draft =
      store.state.draft?.providerId === p.id ? null : store.state.draft;
    store.set({ providers, draft });
  };

  const footBtn = tbtn(
    isDraft() ? "Discard" : "Remove Provider…",
    () => void remove(),
    true,
  );
  const el = h(
    "div",
    { class: `group provider${isDraft() ? " draft" : ""}` },
    h(
      "div",
      { class: "srow provider-head" },
      h("div", { class: "srow-label" }, name, id, saveStatus),
      h("div", { class: "srow-control" }, protocol),
    ),
    h(
      "div",
      { class: "srow" },
      h("div", { class: "srow-label" }, "Base URL"),
      h("div", { class: "srow-control stack" }, baseUrl, endpoint),
    ),
    srow(
      "API key",
      h("div", { class: "srow-inline provider-key" }, key, clearKey),
    ),
    h(
      "div",
      { class: "srow has-block" },
      h("div", { class: "srow-label" }, "Models"),
      h("div", { class: "srow-control srow-inline" }, count, testBtn, fetchBtn),
      h("div", { class: "srow-block" }, models),
    ),
    h("div", { class: "srow provider-foot" }, footBtn),
  );

  const card: ProviderCard = {
    el,
    update(next) {
      p = next;
      if (document.activeElement !== name) name.value = next.name;
      if (document.activeElement !== protocol) protocol.value = next.protocol;
      if (document.activeElement !== baseUrl) baseUrl.value = next.base_url;
      if (document.activeElement !== models) {
        models.value = next.models.join("\n");
        models.rows = rowsFor(next.models.length);
      }
      key.placeholder = next.has_key ? "••••••••" : "Not set";
      clearKey.hidden = !next.has_key;
      id.textContent = next.id;
      paintEndpoint();
      paintCount();
    },
    focus() {
      el.scrollIntoView({ block: "nearest", behavior: "smooth" });
      (isDraft() ? name : baseUrl.value ? key : baseUrl).focus();
    },
  };
  return card;
}
function rowsFor(n: number): number {
  return Math.min(10, Math.max(2, n + 1));
}
