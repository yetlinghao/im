#!/usr/bin/env python3
"""Local stand-in for all three protocols, streaming a fixed reply as SSE.

    scripts/mock_server.py [port]      # default 8787

Endpoints: /v1/chat/completions, /v1/messages, /v1/responses, /v1/models.
Send the word "fail" to get an HTTP 401; "slow" streams at reading speed.
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPLY = (
    "Here's the short version.\n\n"
    "**Streaming** arrives token by token; the UI coalesces deltas to one paint per frame.\n\n"
    "```rust\npub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {\n    self.buf.extend_from_slice(chunk);\n}\n```\n\n"
    "1. Sessions are plain JSON.\n2. Metadata lives on the assistant message.\n\n"
    "That's it."
)
REASONING = "The user wants a compact answer. Lead with the coalescing, then the schema."
# Asked for "html"? Stream a small page instead, slowly, so the live preview can be watched.
HTML_REPLY = (
    "Here's a minimal page:\n\n```html\n<!doctype html>\n<html>\n<head>\n<style>\n"
    "  body { margin: 0; font: 18px/1.5 -apple-system, sans-serif; color: #1d1d1f; background: #fafafa; }\n"
    "  main { max-width: 560px; margin: 10vh auto; padding: 0 24px; }\n"
    "  h1 { font-size: 52px; letter-spacing: -0.03em; margin: 0 0 10px; }\n"
    "  p { color: #6e6e73; }\n"
    "  button { padding: 10px 20px; border-radius: 999px; border: 0; background: #0071e3; color: #fff; font: inherit; }\n"
    "</style>\n</head>\n<body>\n<main>\n  <h1>just chat.</h1>\n"
    "  <p>Rendered live inside the app while the model is still typing.</p>\n"
    "  <button onclick=\"this.textContent='clicked'\">click me</button>\n</main>\n</body>\n</html>\n```\n\nOpen it in a browser."
)


def chunks(text, n=6):
    for i in range(0, len(text), n):
        yield text[i : i + n]


def text_of(content):
    """A message's text, whether `content` is a string or a list of parts."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(p.get("text", "") for p in content if isinstance(p, dict) and "text" in p)
    return ""


def images_in(content):
    if not isinstance(content, list):
        return 0
    return sum(1 for p in content if isinstance(p, dict) and p.get("type") in ("image_url", "image", "input_image"))


def thinking_in(content):
    return isinstance(content, list) and any(isinstance(p, dict) and p.get("type") in ("thinking", "redacted_thinking") for p in content)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("mock: " + fmt % args + "\n")

    def _json(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _sse_start(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()

    def _event(self, data, event=None):
        if event:
            self.wfile.write(f"event: {event}\n".encode())
        self.wfile.write(f"data: {json.dumps(data)}\n\n".encode())
        self.wfile.flush()

    def do_GET(self):
        if self.path.endswith("/models"):
            return self._json(200, {"object": "list", "data": [{"id": "mock-large"}, {"id": "mock-small"}, {"id": "mock-reasoner"}]})
        self._json(404, {"error": {"message": "not found"}})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        msgs = body.get("messages") or body.get("input") or []
        last = text_of(msgs[-1].get("content", "")) if msgs and isinstance(msgs[-1], dict) else ""
        # One line per request showing what came back to us: role, chars, whether
        # earlier replies carried their reasoning (`reasoning_content`, or Anthropic thinking blocks),
        # and `+Ni` for N image parts.
        shape = " ".join(
            f"{m.get('role','?')[0]}{len(text_of(m.get('content','')))}{'+r' if m.get('reasoning_content') or thinking_in(m.get('content')) else ''}{'+%di' % images_in(m.get('content')) if images_in(m.get('content')) else ''}"
            for m in msgs
            if isinstance(m, dict)
        )
        sys.stderr.write(f"mock: {self.path} messages=[{shape}]\n")
        if "fail" in last:
            return self._json(401, {"error": {"message": "Invalid API key (mock)", "type": "authentication_error"}})
        html = "html" in last.lower()
        delay = 0.08 if "slow" in last else 0.03 if html else 0.01
        reply = HTML_REPLY if html else REPLY
        model = body.get("model", "mock")

        if self.path.endswith("/chat/completions"):
            self._sse_start()
            for c in chunks(REASONING):
                self._event({"choices": [{"index": 0, "delta": {"reasoning_content": c}}]})
                time.sleep(delay)
            for c in chunks(reply):
                self._event({"choices": [{"index": 0, "delta": {"content": c}}]})
                time.sleep(delay)
            self._event({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
            self._event({"choices": [], "usage": {"prompt_tokens": 42, "completion_tokens": 97}})
            self.wfile.write(b"data: [DONE]\n\n")
        elif self.path.endswith("/messages"):
            self._sse_start()
            self._event({"type": "message_start", "message": {"model": model, "usage": {"input_tokens": 42, "output_tokens": 1}}}, "message_start")
            self._event({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}, "content_block_start")
            for c in chunks(REASONING):
                self._event({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": c}}, "content_block_delta")
                time.sleep(delay)
            self._event({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "mock-sig"}}, "content_block_delta")
            self._event({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}, "content_block_start")
            for c in chunks(REPLY):
                self._event({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": c}}, "content_block_delta")
                time.sleep(delay)
            self._event({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 97}}, "message_delta")
            self._event({"type": "message_stop"}, "message_stop")
        elif self.path.endswith("/responses"):
            self._sse_start()
            self._event({"type": "response.created", "response": {"id": "resp_1"}}, "response.created")
            for c in chunks(REASONING):
                self._event({"type": "response.reasoning_summary_text.delta", "delta": c}, "response.reasoning_summary_text.delta")
                time.sleep(delay)
            for c in chunks(REPLY):
                self._event({"type": "response.output_text.delta", "delta": c}, "response.output_text.delta")
                time.sleep(delay)
            self._event({"type": "response.completed", "response": {"usage": {"input_tokens": 42, "output_tokens": 97, "output_tokens_details": {"reasoning_tokens": 20}}}}, "response.completed")
        else:
            return self._json(404, {"error": {"message": f"unknown path {self.path}"}})
        self.close_connection = True


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
    print(f"mock server on http://127.0.0.1:{port}/v1", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
