# Developing im

Everything that is not the front door: how to build, use, read the data,
speak the protocols, ship a release. The README stays minimal on purpose.

## Build

Requirements: Rust (stable), Node 22+, and Xcode command line tools on macOS.
For Windows prerequisites and build commands, see [Windows](WINDOWS.md).

```sh
npm ci
npm run tauri dev            # dev build with hot reload
npm run tauri build -- --debug --config .github/tauri-ci.json  # local .app without updater signing
```

## Using it

1. **Settings (⌘, or the gear at the bottom of the sidebar) → Add Provider.**
   Name it, pick the protocol, set the base URL up to and including the
   version segment (`https://api.openai.com/v1`), paste an API key, and either
   type model ids (one per line) or press **Fetch**. Every field saves as soon
   as you leave it — there is no Save button.
2. Close Settings, choose a model from the name at the top of the window
   (⌘K), and type. **Return** sends, **Shift-Return** inserts a newline; when
   an IME composition is active, Return commits the composition instead.
3. While a reply streams, the send button becomes a stop button (**Esc** or
   **⌘.** also stop). Whatever arrived is kept and marked *stopped*.

Hover a message for **copy**, **regenerate** (newest reply) or **edit**
(newest prompt), and under replies the `model · cached/input in · output out ·
ttft · total` line (hover it for the long form); the newest message shows its
line all the time. Right-click for the same as a
native menu. Reasoning, when the provider streams it, appears as a
collapsible *Thought for …* block above the answer. Code blocks have line
numbers and a copy button. An ```html (or ```svg) block renders itself: a preview pane
sits at the top of the block and shows the page live while the code is still
being written beneath it. Click the pane
to enlarge it to the middle of the window — there the page is interactive —
and Esc to put it back. Pages run sandboxed, cut off from the app. While a
reply streams the
view follows it only as long as you stay at the bottom; scroll up and it stays
put, with a small **↓** above the composer to jump back.

### Quick input

**⌥ Space** anywhere on the Mac brings up a small glass field next to the
mouse. If text is selected in the app you are in, it is already there as a
quote; type the question (or nothing — the quote alone is fine), press
**Return**, and im's window comes up with a new chat under way. **Esc** puts
the field away and returns you to what you were doing; clicking anywhere else
does the same. Change or turn off the shortcut under **Settings → General →
Quick input** (click the key cap, press the new keys; ⌫ turns it off). Reading
the selection needs **Accessibility** access — **Settings → General → Quote
selection → Allow…** asks for it; the app is re-signed on every update, so
macOS may ask again after one. Apps that don't expose their selection to
Accessibility (some Electron editors) give no quote; the field still works.

### Keyboard

| Shortcut | Action |
|---|---|
| ⌥ Space (anywhere) | Quick input |
| ⌘N | New chat |
| ⌘⇧A | Attach image (or paste / drop one) |
| ⌘K | Choose model |
| ⌘, | Settings |
| ⌃⌘S | Toggle sidebar |
| ⌥⌘T | Toggle trajectory (right column) |
| ⌘. / Esc | Stop generating |
| ⌘R | Regenerate newest reply |
| ⌘E | Edit newest prompt |
| ⌘⇧[ / ⌘⇧] | Previous / next chat |
| ⌘⇧E | Export chat as JSON |
| ⌘W / red button | Hide the window (click the Dock icon to bring it back) |

### Images

Paste an image into the composer, drop image files onto the window, or use
**File → Attach Image…** (⌘⇧A); they queue as thumbnails above the text until
the message goes out. Anything macOS can decode is accepted (PNG, JPEG, GIF,
WebP, HEIC photos, TIFF…) and normalised before sending: originals up to 2048px
and a few MB are kept as they are, larger or exotic ones are redrawn (PNG stays
PNG so screenshots stay sharp, the rest becomes JPEG). Click an image in the
transcript to enlarge it. Whether a model accepts images is up to the provider;
a text-only model answers with its own error inline.

Chats are renamed by double-clicking them in the sidebar, deleted from the
right-click menu or **File → Delete Chat**. Both side columns can be dragged
wider or narrower at their inner edge, and on a wide window the reading column
itself can be dragged at either edge; double-click an edge to reset.

### Trajectory

The right column (⌥⌘T, or the button at the top right) shows the current chat
as the data it is: the model and provider, a bar with one segment per message
(width = characters, dark = user), then every entry in file order — the system
prompt, each message with its size, and under each reply what that call cost
(`cached/input in · output out · ttft · total`). Click an entry to jump to it in
the transcript. **JSON** switches to the session exactly as stored, with a copy
button; right-click for *Copy Session JSON*, *Copy Messages Only* (just
`{role, content}` pairs) and *Export*.

## Data

Everything lives in `~/Library/Application Support/im/` (**File → Show Data
Folder**). Set `IM_DATA_DIR` to use another location.

```text
settings.json          appearance, default model, system prompt, column widths, quick_shortcut
providers.json         endpoints + model lists      (no secrets)
keys.json              { "<provider id>": "<api key>" }   mode 0600
sessions/<ulid>.json   one chat per file
```

A session:

```json
{
  "schema_version": 1,
  "id": "01k54v8e3c0000000000000000",
  "title": "How does the streaming pipeline work?",
  "created_at": "2026-09-14T12:00:00Z",
  "updated_at": "2026-09-14T12:00:09Z",
  "provider_id": "openrouter",
  "model": "anthropic/claude-sonnet-4",
  "system": "Be terse.",
  "messages": [
    { "role": "user", "content": "How does it work?", "created_at": "2026-09-14T12:00:00Z" },
    {
      "role": "assistant",
      "content": "Bytes come in, events go out…",
      "created_at": "2026-09-14T12:00:09Z",
      "reasoning_content": "The user wants the short version…",
      "meta": {
        "provider_id": "openrouter",
        "protocol": "chat",
        "model": "anthropic/claude-sonnet-4",
        "created_at": "2026-09-14T12:00:09Z",
        "latency_ms": 1840,
        "ttft_ms": 412,
        "thinking_ms": 3200,
        "usage": { "input_tokens": 1283, "cached_input_tokens": 1024, "output_tokens": 236 },
        "finish_reason": "stop"
      }
    }
  ]
}
```

Design rules for the schema:

- `messages[]` is exactly what was (or would be) sent to the model: alternating
  `user` / `assistant` turns with string `content`. Strip everything but
  `role` and `content` and you have a training trajectory. The system prompt
  is a top-level `system` field, copied into the session when it is created so
  the file is self-contained.
- A message from the quick-input panel is a plain string too: the selection as
  a markdown quote (`> …` lines), a blank line, then what was typed. The
  transcript shows the quote as a quote; the model sees ordinary markdown.
- A user message with images has `content` as a list of parts in the Chat
  Completions shape — `{"type": "text", "text": …}` and
  `{"type": "image_url", "image_url": {"url": "data:image/png;base64,…"}}` —
  so it replays as-is on the `chat` protocol and is translated on the wire for
  the other two (below). Images are inlined as `data:` URLs: the file stays
  self-contained, at the cost of size. Replies are always plain strings.
- Everything about *how* a reply was produced hangs off that assistant
  message under `meta` — never off the session or a side table. `finish_reason`
  is the provider's own value (`stop`, `end_turn`, `length`, …) or `cancelled`
  / `error` when the turn did not finish normally; `reasoning_content` is whatever the
  stream carried (Anthropic thinking, DeepSeek `reasoning_content`, Responses
  reasoning summaries), stored verbatim. Anthropic's signed thinking blocks
  are also kept as-is in `thinking_blocks` (`[{"type": "thinking", "thinking",
  "signature"}, {"type": "redacted_thinking", "data"}]`, LiteLLM's field name)
  so they can be sent back. `usage.input_tokens` is the whole
  prompt for that call (Anthropic's cache reads and writes folded back in) and
  `cached_input_tokens` the part the provider served from its prompt cache;
  `ttft_ms` is time to the first streamed token, `latency_ms` the whole call.
  Hover a reply to see them as `model · cached/input in · output out · ttft · total`.
- Regenerate and edit rewrite the newest exchange in place. There is no
  branching, so a file is always one linear conversation.
- Secrets never enter `providers.json` or a session, so those can be shared or
  committed as-is.

**File → Export All Chats as JSONL…** writes one session object per line;
**Export Chat…** writes a single session's JSON.

## Protocols

| `protocol` | Request | Reasoning captured from | Usage from |
|---|---|---|---|
| `chat` | `POST {base_url}/chat/completions`, `stream: true`, `stream_options.include_usage`; earlier replies are replayed with their `reasoning_content`; image parts go through unchanged | `delta.reasoning_content` / `delta.reasoning` | final `usage` chunk |
| `anthropic` | `POST {base_url}/messages`, `stream: true`, `max_tokens` (8192); images become `{"type": "image", "source": {"type": "base64", "media_type", "data"}}` blocks; earlier replies from the same model are sent back as their `thinking_blocks` + a text block | `thinking_delta` (+ `signature_delta`, `redacted_thinking`) | `message_start` + `message_delta` |
| `responses` | `POST {base_url}/responses`, `stream: true`, `store: false`; user images become `input_text` / `input_image` parts | `response.reasoning_summary_text.delta` | `response.completed` |

Model lists come from `GET {base_url}/models` for all three. Requests carry
only the fields above — no sampling or thinking parameters are sent, so any
gateway that implements the protocol works unchanged. If a server answers a
streaming request with a single JSON object instead, it is accepted too.

## Development

```sh
npm run dev                    # frontend alone in a browser, in-memory mock backend
npm run biome:check             # lint, formatting and import order
npm run typecheck               # TypeScript
npm test                       # release feed and checksum regression tests
scripts/snapshot.sh            # render every UI state to build/snapshots/*.png (headless Chrome)
cargo clippy --locked --all-targets --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --locked --manifest-path src-tauri/Cargo.toml # unit tests + local SSE integration tests
scripts/mock_server.py         # local stand-in for all three protocols (port 8787)
scripts/app-snapshot.sh        # launch the debug .app and have it capture its own window
```

`CLAUDE.md` has the architecture map and the rules that keep the app small.
`.github/workflows/ci.yml` runs these checks on PRs and pushes to `main`,
including Rust lint, tests and debug packaging on macOS and Windows. Fork PRs
need no signing secrets. The Windows job also checks that closing the main
window exits the process and uploads a debug installer.

## Install & updates

On macOS:

```sh
curl -fsSL https://im.linghaoz.com/install.sh | sh
```

That downloads the latest release, verifies its checksum and puts `im.app` in
`/Applications` — and because `curl` doesn't set the quarantine flag, it opens
without the "unidentified developer" stop. The macOS install path stays
command-only; there is no dmg.

On Windows 10/11 (x64), download and run
[`im_x64-setup.exe`](https://github.com/yetlinghao/im/releases/latest/download/im_x64-setup.exe).
It installs for the current user. See [Windows](WINDOWS.md) for details.

Updates come from inside the app. It checks the release feed quietly after
launch (and every few hours); when there is a new version a single line
appears at the foot of the sidebar — **Update to 0.2.0** — and clicking it
downloads, installs and relaunches. Nothing pops up. **im → Check for
Updates…** on macOS and **Settings → General → Version** on either platform
are there for checking by hand.

## Releasing

[im.linghaoz.com](https://im.linghaoz.com) is `site/` published by
`.github/workflows/pages.yml` (GitHub Pages, custom domain); it also serves
`install.sh`.

A tag is a release. `.github/workflows/release.yml` builds the universal macOS
app and Windows x64 NSIS installer in parallel, using the project's updater
key to sign both. Once both builds succeed, one publish job assembles:

- `im_universal.app.tar.gz`, `.sig` and `.sha256` (used by `install.sh`).
- `im_x64-setup.exe`, `.sig` and `.sha256` (the site's stable Windows download).
- `latest.json` with `darwin-aarch64`, `darwin-x86_64` and `windows-x86_64`.

The release stays a draft until all assets have been uploaded. Failed builds
leave the previous release current. `scripts/prepare-release.mjs` rejects
missing bundles/signatures and mismatched tag/app versions before publishing.

```sh
# bump versions in src-tauri/tauri.conf.json, src-tauri/Cargo.toml and package.json
# update both lockfiles, commit, then
git tag v0.2.0 && git push origin v0.2.0
```

Two repository secrets are needed once: `TAURI_SIGNING_PRIVATE_KEY` (the
contents of the updater private key — its public half is in
`tauri.conf.json`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (empty if the key
has none). Lose the private key and existing installs can never update again.

The macOS app is ad-hoc signed; there is no Apple Developer ID in the loop.
The Windows installer currently has no Authenticode certificate; updater
signatures are separate from OS code signing. If an Apple identity becomes
available, export `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
`APPLE_PASSWORD`, `APPLE_TEAM_ID` in the workflow and Tauri notarizes as well —
nothing else changes.
