//! Wire protocols. Each protocol is one small module that knows how to build a
//! request body and how to turn one SSE event into zero or more
//! [`StreamEvent`]s. Everything else (HTTP, cancellation, accumulation) is
//! shared and lives here. No Tauri types — this layer is tested headless.

pub mod anthropic;
pub mod chat;
pub mod responses;

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::model::{Message, Protocol, Role, Usage};
use crate::sse::{SseEvent, SseParser};

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Reasoning(String),
    /// Anthropic: closes the thinking block whose text arrived as `Reasoning`
    /// since the previous signature. Without it the block can't be sent back.
    Signature(String),
    /// Anthropic `redacted_thinking`: an opaque `data` blob, sent back as-is.
    RedactedThinking(String),
    Text(String),
    Usage(Usage),
    Finish(Option<String>),
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("{message}")]
    Http { status: u16, message: String },
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Stream(String),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct TurnRequest {
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

/// Result of parsing one SSE event: either events or a provider-reported error.
pub type Parsed = Result<Vec<StreamEvent>, String>;

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(180))
        .user_agent(concat!("im/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client")
}

fn join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// `data:image/png;base64,AAAA` → `("image/png", "AAAA")`; None for anything else.
fn data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    Some((mime, data))
}

/// The conversation's user and assistant turns with their wire role names
/// (the system prompt travels separately).
fn turns(messages: &[Message]) -> impl Iterator<Item = (&'static str, &Message)> + '_ {
    messages.iter().filter_map(|m| match m.role {
        Role::User => Some(("user", m)),
        Role::Assistant => Some(("assistant", m)),
        Role::System => None,
    })
}

fn build(client: &reqwest::Client, req: &TurnRequest) -> reqwest::RequestBuilder {
    match req.protocol {
        Protocol::Chat => chat::build(client, req),
        Protocol::Anthropic => anthropic::build(client, req),
        Protocol::Responses => responses::build(client, req),
    }
}

fn parse(protocol: Protocol, ev: &SseEvent) -> Parsed {
    match protocol {
        Protocol::Chat => chat::parse(ev),
        Protocol::Anthropic => anthropic::parse(ev),
        Protocol::Responses => responses::parse(ev),
    }
}

/// Pull a human-readable message out of an error body, whatever shape it has.
pub fn error_message(status: u16, body: &str) -> String {
    let from_json = serde_json::from_str::<Value>(body).ok().and_then(|v| {
        v.get("error")
            .and_then(|e| e.get("message").and_then(Value::as_str).map(str::to_string).or_else(|| e.as_str().map(str::to_string)))
            .or_else(|| v.get("message").and_then(Value::as_str).map(str::to_string))
            .or_else(|| v.get("detail").and_then(Value::as_str).map(str::to_string))
    });
    match from_json {
        Some(m) if !m.trim().is_empty() => format!("HTTP {status}: {}", m.trim()),
        _ => {
            let snippet: String = body.trim().chars().take(200).collect();
            if snippet.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {snippet}")
            }
        }
    }
}

/// Stream one completion. Every parsed event is handed to `on_event` as soon
/// as it is decoded; the caller accumulates. Returns when the stream ends,
/// the provider reports an error, or `cancel` fires.
pub async fn run(
    client: &reqwest::Client,
    req: &TurnRequest,
    cancel: &CancellationToken,
    mut on_event: impl FnMut(StreamEvent),
) -> Result<(), LlmError> {
    let request = build(client, req);

    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(LlmError::Cancelled),
        r = request.send() => r.map_err(|e| LlmError::Network(describe(&e)))?,
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::Http { status: status.as_u16(), message: error_message(status.as_u16(), &body) });
    }

    let is_sse = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    if !is_sse {
        // Some gateways ignore `stream: true` and answer with one JSON object.
        let body = tokio::select! {
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
            b = response.text() => b.map_err(|e| LlmError::Network(describe(&e)))?,
        };
        let events = match req.protocol {
            Protocol::Chat => chat::parse_complete(&body),
            Protocol::Anthropic => anthropic::parse_complete(&body),
            Protocol::Responses => responses::parse_complete(&body),
        }
        .map_err(LlmError::Stream)?;
        events.into_iter().for_each(&mut on_event);
        return Ok(());
    }

    let mut stream = response.bytes_stream();
    let mut parser = SseParser::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
            c = stream.next() => c,
        };
        match chunk {
            Some(Ok(bytes)) => {
                for ev in parser.push(&bytes) {
                    for e in parse(req.protocol, &ev).map_err(LlmError::Stream)? {
                        on_event(e);
                    }
                }
            }
            Some(Err(e)) => return Err(LlmError::Network(describe(&e))),
            None => break,
        }
    }
    if let Some(ev) = parser.finish() {
        for e in parse(req.protocol, &ev).map_err(LlmError::Stream)? {
            on_event(e);
        }
    }
    Ok(())
}

fn describe(e: &reqwest::Error) -> String {
    // reqwest's Display nests the whole cause chain; the innermost is the useful one.
    let mut msg = e.to_string();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(e);
    while let Some(s) = src {
        msg = s.to_string();
        src = s.source();
    }
    if e.is_timeout() {
        format!("Timed out ({msg})")
    } else if e.is_connect() {
        format!("Could not connect: {msg}")
    } else {
        msg
    }
}

/// `GET {base_url}/models` — the same shape for all three protocols.
pub async fn list_models(
    client: &reqwest::Client,
    protocol: Protocol,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>, LlmError> {
    let url = match protocol {
        Protocol::Anthropic => join(base_url, "models?limit=1000"),
        _ => join(base_url, "models"),
    };
    let mut rb = client.get(&url).header(reqwest::header::ACCEPT, "application/json");
    rb = match protocol {
        Protocol::Anthropic => {
            let rb = rb.header("anthropic-version", anthropic::VERSION);
            match api_key {
                Some(k) => rb.header("x-api-key", k),
                None => rb,
            }
        }
        _ => match api_key {
            Some(k) => rb.bearer_auth(k),
            None => rb,
        },
    };
    let resp = rb.send().await.map_err(|e| LlmError::Network(describe(&e)))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| LlmError::Network(describe(&e)))?;
    if !status.is_success() {
        return Err(LlmError::Http { status: status.as_u16(), message: error_message(status.as_u16(), &body) });
    }
    let v: Value = serde_json::from_str(&body).map_err(|e| LlmError::Stream(format!("bad models response: {e}")))?;
    let items = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut ids: Vec<String> = items
        .iter()
        .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(Value::as_str).map(str::to_string))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_shapes() {
        assert_eq!(error_message(401, r#"{"error":{"message":"bad key","type":"auth"}}"#), "HTTP 401: bad key");
        assert_eq!(error_message(400, r#"{"error":"nope"}"#), "HTTP 400: nope");
        assert_eq!(error_message(500, "<html>oops</html>"), "HTTP 500: <html>oops</html>");
        assert_eq!(error_message(502, ""), "HTTP 502");
    }

    #[test]
    fn join_handles_slashes() {
        assert_eq!(join("https://x/v1/", "/models"), "https://x/v1/models");
        assert_eq!(join("https://x/v1", "chat/completions"), "https://x/v1/chat/completions");
    }
}
