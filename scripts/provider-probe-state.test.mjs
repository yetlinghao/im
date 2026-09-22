import assert from "node:assert/strict";
import test from "node:test";

import {
  isCurrentProviderProbe,
  normalizeFetchedModels,
  resolveProbeCredentials,
  shouldClearApiKeySnapshot,
} from "../src/ui/provider-probe-state.ts";

test("new and draft keys are sent directly without reading the saved key", () => {
  assert.deepEqual(
    resolveProbeCredentials(
      { value: "new-key", version: 1 },
      "",
      "saved-id",
      false,
    ),
    { apiKey: "new-key" },
  );
  assert.deepEqual(
    resolveProbeCredentials({ value: "draft-key", version: 2 }, "", "", true),
    { apiKey: "draft-key" },
  );
});

test("saved key lookup is used only when there is no pending key", () => {
  assert.deepEqual(resolveProbeCredentials(null, "", "saved-id", false), {
    providerId: "saved-id",
  });
  assert.deepEqual(
    resolveProbeCredentials(null, "  typed-key  ", "saved-id", false),
    { apiKey: "typed-key", providerId: "saved-id" },
  );
});

test("clearing a key suppresses fallback to the previously saved key", () => {
  assert.deepEqual(
    resolveProbeCredentials({ value: "", version: 3 }, "", "saved-id", false),
    {},
  );
});

test("only the newest successful key save clears its in-memory snapshot", () => {
  const newest = { value: "newest-key", version: 2 };
  assert.equal(shouldClearApiKeySnapshot(newest, 1), false);
  assert.equal(shouldClearApiKeySnapshot(newest, 2), true);
  assert.equal(shouldClearApiKeySnapshot(null, undefined), false);
});

test("late probes and detached cards cannot update the settings view", () => {
  assert.equal(isCurrentProviderProbe(1, 2, true, "settings"), false);
  assert.equal(isCurrentProviderProbe(2, 2, false, "settings"), false);
  assert.equal(isCurrentProviderProbe(2, 2, true, "chat"), false);
  assert.equal(isCurrentProviderProbe(2, 2, true, "settings"), true);
});

test("Fetch model results are trimmed and deduplicated", () => {
  assert.deepEqual(
    normalizeFetchedModels([" gpt-5 ", "", "gpt-5", "gpt-5-mini"]),
    ["gpt-5", "gpt-5-mini"],
  );
});
