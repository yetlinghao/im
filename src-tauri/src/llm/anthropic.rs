//! Anthropic Messages — `POST {base_url}/messages`, `stream: true`.

use serde_json::{json, Value};

use super::{data_url, join, turns, Parsed, StreamEvent, TurnRequest};
use crate::model::{Content, Message, Part, Protocol, Usage};
use crate::sse::SseEvent;

pub const VERSION: &str = "2023-06-01";

/// Anthropic's content blocks: text stays a string, images become base64 sources.
fn content(c: &Content) -> Value {
    match c {
        Content::Text(t) => Value::String(t.clone()),
        Content::Parts(parts) => Value::Array(
            parts
                .iter()
                .map(|p| match p {
                    Part::Text { text } => json!({ "type": "text", "text": text }),
                    Part::ImageUrl { image_url } => match data_url(&image_url.url) {
                        Some((mime, data)) => json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": data } }),
                        None => json!({ "type": "image", "source": { "type": "url", "url": image_url.url } }),
                    },
                })
                .collect(),
        ),
    }
}

/// An earlier reply goes back with its signed thinking blocks in front of the
/// text, so models that keep prior thinking in context see it. Only when it
/// was produced over this protocol by the model we are calling now: signatures
/// are model-bound, and a thinking block without a valid one is a 400.
fn assistant_content(m: &Message, model: &str) -> Value {
    let text = m.content.text();
    let same_model = m.meta.as_ref().is_some_and(|t| t.protocol == Protocol::Anthropic && t.model == model);
    match &m.thinking_blocks {
        Some(blocks) if same_model && !blocks.is_empty() && !text.is_empty() => {
            let mut parts = blocks.clone();
            parts.push(json!({ "type": "text", "text": text }));
            Value::Array(parts)
        }
        _ => content(&m.content),
    }
}

pub fn build(client: &reqwest::Client, req: &TurnRequest) -> reqwest::RequestBuilder {
    let messages: Vec<Value> = turns(&req.messages)
        .map(|(role, m)| {
            let content = if role == "assistant" { assistant_content(m, &req.model) } else { content(&m.content) };
            json!({ "role": role, "content": content })
        })
        .collect();

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": messages,
        "stream": true,
    });
    if let Some(system) = req.system.as_deref().filter(|s| !s.trim().is_empty()) {
        body["system"] = Value::String(system.to_string());
    }

    let mut rb = client
        .post(join(&req.base_url, "messages"))
        .header("anthropic-version", VERSION)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .json(&body);
    if let Some(key) = &req.api_key {
        rb = rb.header("x-api-key", key);
    }
    rb
}

/// Anthropic's `input_tokens` excludes cache reads and writes; our schema's
/// `input_tokens` is the whole prompt, so fold them back in.
fn usage_from(u: &Value) -> Usage {
    let n = |k: &str| u.get(k).and_then(Value::as_u64);
    let cached = n("cache_read_input_tokens");
    let input = n("input_tokens").map(|i| i + cached.unwrap_or(0) + n("cache_creation_input_tokens").unwrap_or(0));
    Usage {
        input_tokens: input,
        cached_input_tokens: cached.filter(|_| input.is_some()),
        output_tokens: n("output_tokens"),
        reasoning_tokens: None,
    }
}

fn error_text(e: &Value) -> String {
    e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.to_string())
}

pub fn parse(ev: &SseEvent) -> Parsed {
    let data = ev.data.trim();
    if data.is_empty() {
        return Ok(vec![]);
    }
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };
    // Prefer the JSON `type`; the SSE `event:` line mirrors it.
    let kind = v.get("type").and_then(Value::as_str).or(ev.event.as_deref()).unwrap_or("");
    let mut out = Vec::new();
    match kind {
        "error" => {
            return Err(v.get("error").map(error_text).unwrap_or_else(|| "stream error".into()));
        }
        "message_start" => {
            if let Some(u) = v.pointer("/message/usage") {
                out.push(StreamEvent::Usage(usage_from(u)));
            }
        }
        "content_block_delta" => {
            if let Some(delta) = v.get("delta") {
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = delta.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                            out.push(StreamEvent::Text(t.to_string()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = delta.get("thinking").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                            out.push(StreamEvent::Reasoning(t.to_string()));
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(s) = delta.get("signature").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                            out.push(StreamEvent::Signature(s.to_string()));
                        }
                    }
                    _ => {} // input_json_delta (tools) — not ours
                }
            }
        }
        "content_block_start" => {
            let block = v.get("content_block");
            if block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("redacted_thinking") {
                if let Some(d) = block.and_then(|b| b.get("data")).and_then(Value::as_str) {
                    out.push(StreamEvent::RedactedThinking(d.to_string()));
                }
            }
        }
        "message_delta" => {
            if let Some(reason) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                out.push(StreamEvent::Finish(Some(reason.to_string())));
            }
            if let Some(u) = v.get("usage") {
                // Only output_tokens is authoritative here; input came with message_start.
                out.push(StreamEvent::Usage(Usage { output_tokens: u.get("output_tokens").and_then(Value::as_u64), ..Usage::default() }));
            }
        }
        _ => {} // ping, content_block_stop, message_stop
    }
    Ok(out)
}

/// A non-streamed `message` object.
pub fn parse_complete(body: &str) -> Result<Vec<StreamEvent>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("unexpected response: {e}"))?;
    if v.get("type").and_then(Value::as_str) == Some("error") {
        return Err(v.get("error").map(error_text).unwrap_or_else(|| "error".into()));
    }
    let mut out = Vec::new();
    for block in v.get("content").and_then(Value::as_array).into_iter().flatten() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    out.push(StreamEvent::Text(t.to_string()));
                }
            }
            Some("thinking") => {
                if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                    out.push(StreamEvent::Reasoning(t.to_string()));
                }
                if let Some(s) = block.get("signature").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    out.push(StreamEvent::Signature(s.to_string()));
                }
            }
            Some("redacted_thinking") => {
                if let Some(d) = block.get("data").and_then(Value::as_str) {
                    out.push(StreamEvent::RedactedThinking(d.to_string()));
                }
            }
            _ => {}
        }
    }
    out.push(StreamEvent::Finish(v.get("stop_reason").and_then(Value::as_str).map(str::to_string)));
    if let Some(u) = v.get("usage") {
        out.push(StreamEvent::Usage(usage_from(u)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Role, TurnMeta};

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent { event: Some(event.to_string()), data: data.to_string() }
    }

    #[test]
    fn images_become_base64_source_blocks() {
        let c = Content::with_images("see".into(), vec!["data:image/jpeg;base64,/9j/".into()]);
        let v = content(&c);
        assert_eq!(v[0]["type"], "text");
        assert_eq!(v[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(v[1]["source"]["data"], "/9j/");
        assert_eq!(content(&Content::Text("t".into())), "t");
    }

    #[test]
    fn cache_reads_fold_into_input_tokens() {
        let start = parse(&ev("message_start", r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":5,"cache_read_input_tokens":20,"cache_creation_input_tokens":3,"output_tokens":1}}}"#)).unwrap();
        assert_eq!(start, vec![StreamEvent::Usage(Usage { input_tokens: Some(28), cached_input_tokens: Some(20), output_tokens: Some(1), reasoning_tokens: None })]);
    }

    #[test]
    fn full_stream() {
        let start = parse(&ev("message_start", r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":25,"output_tokens":1}}}"#)).unwrap();
        assert_eq!(start, vec![StreamEvent::Usage(Usage { input_tokens: Some(25), cached_input_tokens: None, output_tokens: Some(1), reasoning_tokens: None })]);

        assert!(parse(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#)).unwrap().is_empty());
        let th = parse(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me"}}"#)).unwrap();
        assert_eq!(th, vec![StreamEvent::Reasoning("Let me".into())]);
        let sig = parse(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#)).unwrap();
        assert_eq!(sig, vec![StreamEvent::Signature("abc".into())]);

        let tx = parse(&ev("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello"}}"#)).unwrap();
        assert_eq!(tx, vec![StreamEvent::Text("Hello".into())]);

        let end = parse(&ev("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#)).unwrap();
        assert_eq!(end[0], StreamEvent::Finish(Some("end_turn".into())));
        assert_eq!(end[1], StreamEvent::Usage(Usage { output_tokens: Some(15), ..Usage::default() }));

        assert!(parse(&ev("ping", r#"{"type":"ping"}"#)).unwrap().is_empty());
        assert!(parse(&ev("message_stop", r#"{"type":"message_stop"}"#)).unwrap().is_empty());
    }

    #[test]
    fn error_event() {
        let e = parse(&ev("error", r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)).unwrap_err();
        assert_eq!(e, "Overloaded");
    }

    #[test]
    fn body_shape() {
        let req = TurnRequest {
            protocol: Protocol::Anthropic,
            base_url: "http://h/v1/".into(),
            api_key: Some("k".into()),
            model: "claude".into(),
            system: Some("sys".into()),
            messages: vec![
                Message::user("hi", "t".into()),
                Message { role: Role::Assistant, content: "yo".into(), created_at: None, reasoning_content: None, thinking_blocks: None, meta: None },
                Message::user("more", "t".into()),
            ],
            max_tokens: 4096,
        };
        let r = build(&reqwest::Client::new(), &req).build().unwrap();
        assert_eq!(r.url().as_str(), "http://h/v1/messages");
        assert_eq!(r.headers()["x-api-key"], "k");
        assert_eq!(r.headers()["anthropic-version"], VERSION);
        let body: Value = serde_json::from_slice(r.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["system"], "sys");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert!(body["messages"].as_array().unwrap().iter().all(|m| m["role"] != "system"));
    }

    #[test]
    fn redacted_thinking_block() {
        let r = parse(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"EmwK"}}"#)).unwrap();
        assert_eq!(r, vec![StreamEvent::RedactedThinking("EmwK".into())]);
    }

    fn reply(model: &str, blocks: Option<Vec<Value>>, text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: text.into(),
            created_at: None,
            reasoning_content: Some("why".into()),
            thinking_blocks: blocks,
            meta: Some(TurnMeta { protocol: Protocol::Anthropic, model: model.into(), ..TurnMeta::default() }),
        }
    }

    fn sent_assistant(m: Message) -> Value {
        let req = TurnRequest {
            protocol: Protocol::Anthropic,
            base_url: "http://h/v1".into(),
            api_key: None,
            model: "claude".into(),
            system: None,
            messages: vec![Message::user("hi", "t".into()), m, Message::user("more", "t".into())],
            max_tokens: 4096,
        };
        let r = build(&reqwest::Client::new(), &req).build().unwrap();
        let body: Value = serde_json::from_slice(r.body().unwrap().as_bytes().unwrap()).unwrap();
        body["messages"][1]["content"].clone()
    }

    #[test]
    fn signed_thinking_is_replayed_for_the_same_model_only() {
        let blocks = vec![json!({ "type": "thinking", "thinking": "why", "signature": "s" }), json!({ "type": "redacted_thinking", "data": "d" })];
        assert_eq!(
            sent_assistant(reply("claude", Some(blocks.clone()), "yo")),
            json!([blocks[0], blocks[1], { "type": "text", "text": "yo" }])
        );
        // Another model's signature is useless at best; an empty reply can't carry a text block.
        assert_eq!(sent_assistant(reply("other", Some(blocks.clone()), "yo")), "yo");
        assert_eq!(sent_assistant(reply("claude", Some(blocks), "")), "");
        assert_eq!(sent_assistant(reply("claude", None, "yo")), "yo");
    }

    #[test]
    fn complete_object_keeps_signatures() {
        let out = parse_complete(r#"{"type":"message","content":[{"type":"thinking","thinking":"hm","signature":"s"},{"type":"redacted_thinking","data":"d"},{"type":"text","text":"Hi"}],"stop_reason":"end_turn"}"#).unwrap();
        assert_eq!(
            out[..4],
            [StreamEvent::Reasoning("hm".into()), StreamEvent::Signature("s".into()), StreamEvent::RedactedThinking("d".into()), StreamEvent::Text("Hi".into())]
        );
    }

    #[test]
    fn complete_object() {
        let out = parse_complete(r#"{"type":"message","content":[{"type":"text","text":"Hi"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#).unwrap();
        assert_eq!(out[0], StreamEvent::Text("Hi".into()));
        assert_eq!(out[1], StreamEvent::Finish(Some("end_turn".into())));
    }
}
