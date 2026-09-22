//! OpenAI Chat Completions — `POST {base_url}/chat/completions`, `stream: true`.
//! Also what OpenRouter, DeepSeek, Groq, Ollama, vLLM, … speak.

use serde_json::{json, Value};

use super::{join, turns, Parsed, StreamEvent, TurnRequest};
use crate::model::Usage;
use crate::sse::SseEvent;

pub fn build(client: &reqwest::Client, req: &TurnRequest) -> reqwest::RequestBuilder {
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = req.system.as_deref().filter(|s| !s.trim().is_empty()) {
        messages.push(json!({ "role": "system", "content": system }));
    }
    // Thinking models (DeepSeek, MiMo, Kimi, …) want their earlier reasoning
    // back under the same field they stream it in; replaying it is what
    // keeps a multi-turn trajectory faithful.
    messages.extend(turns(&req.messages).map(|(role, m)| match &m.reasoning_content {
        Some(r) if role == "assistant" && !r.is_empty() => json!({ "role": role, "content": m.content, "reasoning_content": r }),
        _ => json!({ "role": role, "content": m.content }),
    }));

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if req.probe { body["max_tokens"] = json!(req.max_tokens); }

    let mut rb = client
        .post(join(&req.base_url, "chat/completions"))
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .json(&body);
    if let Some(key) = &req.api_key {
        rb = rb.bearer_auth(key);
    }
    rb
}

fn usage_from(v: &Value) -> Option<Usage> {
    let u = v.get("usage")?;
    if u.is_null() {
        return None;
    }
    Some(Usage {
        input_tokens: u.get("prompt_tokens").and_then(Value::as_u64),
        // OpenAI nests it; DeepSeek reports a top-level cache hit count.
        cached_input_tokens: u
            .pointer("/prompt_tokens_details/cached_tokens")
            .or_else(|| u.get("prompt_cache_hit_tokens"))
            .and_then(Value::as_u64),
        output_tokens: u.get("completion_tokens").and_then(Value::as_u64),
        reasoning_tokens: u
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
    })
}

fn error_in(v: &Value) -> Option<String> {
    let e = v.get("error")?;
    Some(
        e.get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| e.to_string()),
    )
}

pub fn parse(ev: &SseEvent) -> Parsed {
    let data = ev.data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(vec![]);
    }
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return Ok(vec![]), // keep-alives and gateway chatter
    };
    if let Some(msg) = error_in(&v) {
        return Err(msg);
    }
    let mut out = Vec::new();
    if let Some(choice) = v.get("choices").and_then(Value::as_array).and_then(|c| c.first()) {
        if let Some(delta) = choice.get("delta") {
            // DeepSeek/OpenRouter use `reasoning_content`, others `reasoning`.
            for key in ["reasoning_content", "reasoning"] {
                if let Some(r) = delta.get(key).and_then(Value::as_str) {
                    if !r.is_empty() {
                        out.push(StreamEvent::Reasoning(r.to_string()));
                    }
                }
            }
            if let Some(t) = delta.get("content").and_then(Value::as_str) {
                if !t.is_empty() {
                    out.push(StreamEvent::Text(t.to_string()));
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            out.push(StreamEvent::Finish(Some(reason.to_string())));
        }
    }
    if let Some(u) = usage_from(&v) {
        out.push(StreamEvent::Usage(u));
    }
    Ok(out)
}

/// A non-streamed `chat.completion` object.
pub fn parse_complete(body: &str) -> Result<Vec<StreamEvent>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("unexpected response: {e}"))?;
    if let Some(msg) = error_in(&v) {
        return Err(msg);
    }
    let mut out = Vec::new();
    if let Some(choice) = v.get("choices").and_then(Value::as_array).and_then(|c| c.first()) {
        let msg = choice.get("message").cloned().unwrap_or(Value::Null);
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = msg.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                out.push(StreamEvent::Reasoning(r.to_string()));
            }
        }
        if let Some(t) = msg.get("content").and_then(Value::as_str) {
            out.push(StreamEvent::Text(t.to_string()));
        }
        out.push(StreamEvent::Finish(choice.get("finish_reason").and_then(Value::as_str).map(str::to_string)));
    }
    if let Some(u) = usage_from(&v) {
        out.push(StreamEvent::Usage(u));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, Protocol, Role};

    fn ev(data: &str) -> SseEvent {
        SseEvent { event: None, data: data.to_string() }
    }

    #[test]
    fn cached_prompt_tokens_openai_and_deepseek() {
        let p = parse(&ev(r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":8}}}"#)).unwrap();
        assert_eq!(p, vec![StreamEvent::Usage(Usage { input_tokens: Some(10), cached_input_tokens: Some(8), output_tokens: Some(1), reasoning_tokens: None })]);
        let p = parse(&ev(r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":1,"prompt_cache_hit_tokens":6,"prompt_cache_miss_tokens":4}}"#)).unwrap();
        assert_eq!(p, vec![StreamEvent::Usage(Usage { input_tokens: Some(10), cached_input_tokens: Some(6), output_tokens: Some(1), reasoning_tokens: None })]);
    }

    #[test]
    fn deltas_finish_and_usage() {
        let p = parse(&ev(r#"{"id":"x","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"},"finish_reason":null}]}"#)).unwrap();
        assert_eq!(p, vec![StreamEvent::Text("Hel".into())]);

        let p = parse(&ev(r#"{"choices":[{"index":0,"delta":{"reasoning_content":"hmm"},"finish_reason":null}]}"#)).unwrap();
        assert_eq!(p, vec![StreamEvent::Reasoning("hmm".into())]);

        let p = parse(&ev(r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#)).unwrap();
        assert_eq!(p, vec![StreamEvent::Finish(Some("stop".into()))]);

        let p = parse(&ev(r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":2}}}"#)).unwrap();
        assert_eq!(
            p,
            vec![StreamEvent::Usage(Usage { input_tokens: Some(10), cached_input_tokens: None, output_tokens: Some(5), reasoning_tokens: Some(2) })]
        );

        assert!(parse(&ev("[DONE]")).unwrap().is_empty());
        assert!(parse(&ev(": ping")).unwrap().is_empty());
    }

    #[test]
    fn error_events_surface() {
        let e = parse(&ev(r#"{"error":{"message":"rate limited","code":429}}"#)).unwrap_err();
        assert_eq!(e, "rate limited");
    }

    #[test]
    fn body_shape() {
        let req = TurnRequest {
            protocol: Protocol::Chat,
            base_url: "http://h/v1".into(),
            api_key: Some("k".into()),
            model: "m".into(),
            system: Some("sys".into()),
            messages: vec![Message::user("hi", "t".into())],
            max_tokens: 100,
            probe: false,
        };
        let r = build(&reqwest::Client::new(), &req).build().unwrap();
        assert_eq!(r.url().as_str(), "http://h/v1/chat/completions");
        assert_eq!(r.headers()["authorization"], "Bearer k");
        let body: Value = serde_json::from_slice(r.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert!(body["messages"][1].get("reasoning_content").is_none());
        assert_eq!(body["stream"], true);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn earlier_reasoning_is_replayed() {
        let mut reply = Message { role: Role::Assistant, content: "yo".into(), created_at: None, reasoning_content: Some("why".into()), meta: None };
        let req = TurnRequest {
            protocol: Protocol::Chat,
            base_url: "http://h/v1".into(),
            api_key: None,
            model: "m".into(),
            system: None,
            messages: vec![Message::user("hi", "t".into()), reply.clone(), Message::user("more", "t".into())],
            max_tokens: 100,
            probe: false,
        };
        let r = build(&reqwest::Client::new(), &req).build().unwrap();
        let body: Value = serde_json::from_slice(r.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["messages"][1]["reasoning_content"], "why");
        assert!(body["messages"][0].get("reasoning_content").is_none());

        reply.reasoning_content = Some(String::new());
        let req = TurnRequest { messages: vec![reply], ..req };
        let r = build(&reqwest::Client::new(), &req).build().unwrap();
        let body: Value = serde_json::from_slice(r.body().unwrap().as_bytes().unwrap()).unwrap();
        assert!(body["messages"][0].get("reasoning_content").is_none());
    }

    #[test]
    fn complete_object() {
        let out = parse_complete(r#"{"choices":[{"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#).unwrap();
        assert_eq!(out[0], StreamEvent::Text("Hi".into()));
        assert_eq!(out[1], StreamEvent::Finish(Some("stop".into())));
        assert!(matches!(out[2], StreamEvent::Usage(_)));
    }
}
