//! OpenAI Responses — `POST {base_url}/responses`, `stream: true`.

use serde_json::{json, Value};

use super::{join, turns, Parsed, StreamEvent, TurnRequest};
use crate::model::{Content, Part, Usage};
use crate::sse::SseEvent;

/// Responses input items: `input_text` / `input_image` parts for the user,
/// plain text for earlier assistant turns.
fn content(role: &str, c: &Content) -> Value {
    match c {
        Content::Text(t) => Value::String(t.clone()),
        Content::Parts(parts) if role == "user" => Value::Array(
            parts
                .iter()
                .map(|p| match p {
                    Part::Text { text } => json!({ "type": "input_text", "text": text }),
                    Part::ImageUrl { image_url } => json!({ "type": "input_image", "image_url": image_url.url }),
                })
                .collect(),
        ),
        Content::Parts(_) => Value::String(c.text()),
    }
}

pub fn build(client: &reqwest::Client, req: &TurnRequest) -> reqwest::RequestBuilder {
    let input: Vec<Value> =
        turns(&req.messages).map(|(role, m)| json!({ "role": role, "content": content(role, &m.content) })).collect();

    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": true,
        "store": false,
    });
    if req.probe { body["max_output_tokens"] = json!(req.max_tokens); }
    if let Some(system) = req.system.as_deref().filter(|s| !s.trim().is_empty()) {
        body["instructions"] = Value::String(system.to_string());
    }

    let mut rb = client
        .post(join(&req.base_url, "responses"))
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .json(&body);
    if let Some(key) = &req.api_key {
        rb = rb.bearer_auth(key);
    }
    rb
}

fn usage_from(u: &Value) -> Usage {
    Usage {
        input_tokens: u.get("input_tokens").and_then(Value::as_u64),
        cached_input_tokens: u.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_u64),
        output_tokens: u.get("output_tokens").and_then(Value::as_u64),
        reasoning_tokens: u.pointer("/output_tokens_details/reasoning_tokens").and_then(Value::as_u64),
    }
}

fn error_text(e: &Value) -> String {
    e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.to_string())
}

pub fn parse(ev: &SseEvent) -> Parsed {
    let data = ev.data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(vec![]);
    }
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };
    let kind = v.get("type").and_then(Value::as_str).or(ev.event.as_deref()).unwrap_or("");
    let mut out = Vec::new();
    match kind {
        "response.output_text.delta" => {
            if let Some(t) = v.get("delta").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                out.push(StreamEvent::Text(t.to_string()));
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(t) = v.get("delta").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                out.push(StreamEvent::Reasoning(t.to_string()));
            }
        }
        // Summaries arrive in parts; separate them so they read as paragraphs.
        "response.reasoning_summary_part.done" => out.push(StreamEvent::Reasoning("\n\n".into())),
        "response.completed" | "response.incomplete" => {
            let resp = v.get("response").cloned().unwrap_or(Value::Null);
            let reason = if kind == "response.completed" {
                Some("stop".to_string())
            } else {
                resp.pointer("/incomplete_details/reason").and_then(Value::as_str).map(str::to_string).or(Some("incomplete".into()))
            };
            if let Some(u) = resp.get("usage") {
                out.push(StreamEvent::Usage(usage_from(u)));
            }
            out.push(StreamEvent::Finish(reason));
        }
        "response.failed" => {
            let msg = v.pointer("/response/error").map(error_text).unwrap_or_else(|| "response failed".into());
            return Err(msg);
        }
        "error" => {
            return Err(v.get("error").map(error_text).or_else(|| v.get("message").and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| "stream error".into()));
        }
        _ => {} // response.created, in_progress, output_item.*, content_part.*, output_text.done …
    }
    Ok(out)
}

/// A non-streamed `response` object.
pub fn parse_complete(body: &str) -> Result<Vec<StreamEvent>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("unexpected response: {e}"))?;
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        return Err(error_text(e));
    }
    let mut out = Vec::new();
    for item in v.get("output").and_then(Value::as_array).into_iter().flatten() {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item.get("content").and_then(Value::as_array).into_iter().flatten() {
                    if part.get("type").and_then(Value::as_str) == Some("output_text") {
                        if let Some(t) = part.get("text").and_then(Value::as_str) {
                            out.push(StreamEvent::Text(t.to_string()));
                        }
                    }
                }
            }
            Some("reasoning") => {
                for part in item.get("summary").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        out.push(StreamEvent::Reasoning(format!("{t}\n\n")));
                    }
                }
            }
            _ => {}
        }
    }
    out.push(StreamEvent::Finish(
        v.pointer("/incomplete_details/reason").and_then(Value::as_str).map(str::to_string).or(Some("stop".into())),
    ));
    if let Some(u) = v.get("usage") {
        out.push(StreamEvent::Usage(usage_from(u)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Message, Protocol};

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent { event: Some(event.to_string()), data: data.to_string() }
    }

    #[test]
    fn user_images_are_input_image_parts() {
        let c = Content::with_images("see".into(), vec!["data:image/png;base64,AAAA".into()]);
        let v = content("user", &c);
        assert_eq!(v[0]["type"], "input_text");
        assert_eq!(v[1]["type"], "input_image");
        assert_eq!(v[1]["image_url"], "data:image/png;base64,AAAA");
        assert_eq!(content("assistant", &c), "see");
    }

    #[test]
    fn full_stream() {
        assert!(parse(&ev("response.created", r#"{"type":"response.created","response":{"id":"r"}}"#)).unwrap().is_empty());
        let r = parse(&ev("response.reasoning_summary_text.delta", r#"{"type":"response.reasoning_summary_text.delta","delta":"Think"}"#)).unwrap();
        assert_eq!(r, vec![StreamEvent::Reasoning("Think".into())]);
        let t = parse(&ev("response.output_text.delta", r#"{"type":"response.output_text.delta","item_id":"i","delta":"Hi"}"#)).unwrap();
        assert_eq!(t, vec![StreamEvent::Text("Hi".into())]);
        let done = parse(&ev("response.completed", r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":7,"output_tokens":9,"output_tokens_details":{"reasoning_tokens":4}}}}"#)).unwrap();
        assert_eq!(done[0], StreamEvent::Usage(Usage { input_tokens: Some(7), cached_input_tokens: None, output_tokens: Some(9), reasoning_tokens: Some(4) }));
        assert_eq!(done[1], StreamEvent::Finish(Some("stop".into())));
    }

    #[test]
    fn incomplete_and_failed() {
        let inc = parse(&ev("response.incomplete", r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#)).unwrap();
        assert_eq!(inc, vec![StreamEvent::Finish(Some("max_output_tokens".into()))]);
        let f = parse(&ev("response.failed", r#"{"type":"response.failed","response":{"error":{"code":"server_error","message":"boom"}}}"#)).unwrap_err();
        assert_eq!(f, "boom");
        let e = parse(&ev("error", r#"{"type":"error","code":"x","message":"bad","param":null}"#)).unwrap_err();
        assert_eq!(e, "bad");
    }

    #[test]
    fn body_shape() {
        let req = TurnRequest {
            protocol: Protocol::Responses,
            base_url: "http://h/v1".into(),
            api_key: None,
            model: "gpt".into(),
            system: None,
            messages: vec![Message::user("hi", "t".into())],
            max_tokens: 100,
            probe: false,
        };
        let r = build(&reqwest::Client::new(), &req).build().unwrap();
        assert_eq!(r.url().as_str(), "http://h/v1/responses");
        assert!(r.headers().get("authorization").is_none());
        let body: Value = serde_json::from_slice(r.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["input"][0]["role"], "user");
        assert!(body.get("instructions").is_none());
        assert_eq!(body["store"], false);
    }

    #[test]
    fn complete_object() {
        let out = parse_complete(r#"{"id":"r","output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"why"}]},{"type":"message","content":[{"type":"output_text","text":"Hi"}]}],"usage":{"input_tokens":1,"output_tokens":2}}"#).unwrap();
        assert_eq!(out[0], StreamEvent::Reasoning("why\n\n".into()));
        assert_eq!(out[1], StreamEvent::Text("Hi".into()));
        assert_eq!(out[2], StreamEvent::Finish(Some("stop".into())));
    }
}
