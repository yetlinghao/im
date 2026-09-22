//! Wire protocols. Each protocol is one small module that knows how to build a
//! request body and how to turn one SSE event into zero or more
//! [`StreamEvent`]s. Everything else (HTTP, cancellation, accumulation) is
//! shared and lives here. No Tauri types — this layer is tested headless.

pub mod anthropic;
pub mod chat;
pub mod responses;

use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::model::{Message, Protocol, Role, Usage};
use crate::sse::{SseEvent, SseParser};

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Reasoning(String),
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
    #[error("{message}")]
    Stream { status: u16, message: String },
    #[error("{message}")]
    ProtocolResponse { status: u16, message: String },
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
    /// Probe requests carry protocol-specific output limits only.
    pub probe: bool,
}

/// Result of parsing one SSE event: either events or a provider-reported error.
pub type Parsed = Result<Vec<StreamEvent>, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbePhase { Validation, Models, Stream }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeErrorKind { InvalidUrl, Connection, Tls, Timeout, Authentication, Endpoint, RateLimited, ModelUnavailable, UnexpectedResponse, IncompleteStream, Provider }

#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    pub phase: ProbePhase,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub model_count: Option<usize>,
    pub stream_ok: bool,
    pub error_category: Option<ProbeErrorKind>,
    pub message: String,
    /// Sanitized detail only: never includes provider response bodies or URLs.
    pub detail: Option<String>,
    pub models_warning: Option<String>,
    pub models: Option<Vec<String>>,
}

pub struct ModelsResponse { pub models: Vec<String>, pub status: u16 }

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Validate and normalize a provider base URL without adding a scheme or API version.
/// Localhost and private network hosts intentionally remain valid.
pub fn normalize_base_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() { return Err("Base URL is required.".into()); }
    let url = reqwest::Url::parse(input)
        .map_err(|_| "Enter a valid URL beginning with http:// or https://.".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Base URL must use http:// or https://.".into());
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("Remove credentials, query parameters, and fragments from the Base URL.".into());
    }
    let path = url.path().trim_end_matches('/');
    let last = path.rsplit('/').next().unwrap_or_default().to_ascii_lowercase();
    if matches!(last.as_str(), "models" | "messages" | "responses" | "completions") {
        return Err("Enter the API base URL (for example, ending in /v1), not an endpoint path.".into());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}
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
) -> Result<u16, LlmError> {
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
        if req.probe { return Err(LlmError::ProtocolResponse { status: status.as_u16(), message: "expected a text/event-stream response".into() }); }
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
        .map_err(|message| LlmError::ProtocolResponse { status: status.as_u16(), message })?;
        events.into_iter().for_each(&mut on_event);
        return Ok(status.as_u16());
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
                    for e in parse(req.protocol, &ev).map_err(|message| LlmError::Stream { status: status.as_u16(), message })? {
                        on_event(e);
                    }
                }
            }
            Some(Err(e)) => return Err(LlmError::Network(describe(&e))),
            None => break,
        }
    }
    if let Some(ev) = parser.finish() {
        for e in parse(req.protocol, &ev).map_err(|message| LlmError::Stream { status: status.as_u16(), message })? {
            on_event(e);
        }
    }
    Ok(status.as_u16())
}

fn probe_message(kind: ProbeErrorKind) -> &'static str {
    match kind {
        ProbeErrorKind::InvalidUrl => "The Base URL is invalid.",
        ProbeErrorKind::Connection => "Could not connect to the provider.",
        ProbeErrorKind::Tls => "The secure connection failed.",
        ProbeErrorKind::Timeout => "The provider request timed out.",
        ProbeErrorKind::Authentication => "The API key is invalid or lacks permission.",
        ProbeErrorKind::Endpoint => "The endpoint path was not found.",
        ProbeErrorKind::RateLimited => "The provider is rate limited or out of quota.",
        ProbeErrorKind::ModelUnavailable => "The model does not exist or is not available to this key.",
        ProbeErrorKind::UnexpectedResponse => "The provider returned an unexpected response for this protocol.",
        ProbeErrorKind::IncompleteStream => "The stream ended without a valid completion event.",
        ProbeErrorKind::Provider => "The provider rejected the request.",
    }
}

fn classify_error(error: &LlmError) -> (ProbeErrorKind, Option<u16>, Option<String>) {
    match error {
        LlmError::Http { status, message } => {
            let lower = message.to_ascii_lowercase();
            let missing_model = lower.contains("model") && ["not found", "does not exist", "unavailable", "unknown"].iter().any(|s| lower.contains(s));
            let kind = match *status {
                401 | 403 => ProbeErrorKind::Authentication,
                404 if missing_model => ProbeErrorKind::ModelUnavailable,
                404 => ProbeErrorKind::Endpoint,
                429 => ProbeErrorKind::RateLimited,
                400 | 422 if missing_model => ProbeErrorKind::ModelUnavailable,
                _ => ProbeErrorKind::Provider,
            };
            (kind, Some(*status), Some(format!("HTTP {status}")))
        }
        LlmError::Network(message) => {
            let lower = message.to_ascii_lowercase();
            let kind = if lower.contains("timed out") || lower.contains("timeout") { ProbeErrorKind::Timeout }
                else if lower.contains("tls") || lower.contains("certificate") || lower.contains("cert ") { ProbeErrorKind::Tls }
                else { ProbeErrorKind::Connection };
            let detail = match kind {
                ProbeErrorKind::Timeout => "Request timed out".to_string(),
                ProbeErrorKind::Tls => "TLS handshake failed".to_string(),
                _ => "Connection failed".to_string(),
            };
            (kind, None, Some(detail))
        }
        LlmError::Stream { status, message } => {
            let lower = message.to_ascii_lowercase();
            let missing_model = lower.contains("model")
                && ["not found", "does not exist", "unavailable", "unknown", "invalid"]
                    .iter()
                    .any(|term| lower.contains(term));
            let kind = if [
                "authentication",
                "unauthorized",
                "invalid api key",
                "incorrect api key",
                "invalid_api_key",
                "permission",
            ]
            .iter()
            .any(|term| lower.contains(term))
            {
                ProbeErrorKind::Authentication
            } else if [
                "rate limit",
                "rate_limit",
                "too many requests",
                "quota",
                "billing",
                "insufficient_quota",
            ]
            .iter()
            .any(|term| lower.contains(term))
            {
                ProbeErrorKind::RateLimited
            } else if missing_model {
                ProbeErrorKind::ModelUnavailable
            } else {
                ProbeErrorKind::Provider
            };
            (
                kind,
                Some(*status),
                Some(format!("HTTP {status}: provider stream error")),
            )
        }
        LlmError::ProtocolResponse { status, message } => {
            let lower = message.to_ascii_lowercase();
            let kind = if lower.contains("model") && ["not found", "does not exist", "unavailable", "unknown"].iter().any(|s| lower.contains(s)) { ProbeErrorKind::ModelUnavailable } else { ProbeErrorKind::UnexpectedResponse };
            (kind, Some(*status), Some(format!("HTTP {status}: protocol response was invalid")))
        },
        LlmError::Cancelled => (ProbeErrorKind::Provider, None, None),
    }
}

fn failed_probe(
    phase: ProbePhase,
    started: std::time::Instant,
    kind: ProbeErrorKind,
    status: Option<u16>,
    detail: Option<String>,
    models: Option<Vec<String>>,
    models_warning: Option<String>,
) -> ProbeResult {
    ProbeResult {
        ok: false,
        phase,
        status,
        duration_ms: started.elapsed().as_millis() as u64,
        model_count: models.as_ref().map(Vec::len),
        stream_ok: false,
        error_category: Some(kind),
        message: probe_message(kind).into(),
        detail,
        models_warning,
        models,
    }
}

/// Runs a bounded provider check without creating or mutating a session.
pub async fn probe(
    client: &reqwest::Client,
    protocol: Protocol,
    base_url: &str,
    api_key: Option<&str>,
    model: Option<&str>,
) -> ProbeResult {
    let started = std::time::Instant::now();
    let base_url = match normalize_base_url(base_url) {
        Ok(url) => url,
        Err(_) => return failed_probe(ProbePhase::Validation, started, ProbeErrorKind::InvalidUrl, None, None, None, None),
    };
    let listed = tokio::time::timeout(PROBE_TIMEOUT, list_models_detailed(client, protocol, &base_url, api_key)).await;
    let (models, status, list_error) = match listed {
        Ok(Ok(result)) => (Some(result.models), Some(result.status), None),
        Ok(Err(error)) => {
            let (kind, status, _) = classify_error(&error);
            (None, status, Some((kind, status)))
        }
        Err(_) => (None, None, Some((ProbeErrorKind::Timeout, None))),
    };
    let model = match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(model) => model,
        None => {
            if let Some((kind, status)) = list_error {
                let detail = status.map(|s| format!("HTTP {s}")).or_else(|| Some(probe_message(kind).into()));
                return failed_probe(ProbePhase::Models, started, kind, status, detail, models, None);
            }
            let count = models.as_ref().map_or(0, Vec::len);
            return ProbeResult {
                ok: true,
                phase: ProbePhase::Models,
                status,
                duration_ms: started.elapsed().as_millis() as u64,
                model_count: Some(count),
                stream_ok: false,
                error_category: None,
                message: if count == 0 { "Connection successful; no models were returned.".into() } else { format!("Connection successful; {count} models found.") },
                detail: None,
                models_warning: None,
                models,
            };
        }
    };
    let warning = list_error.map(|(kind, status)| match status {
        Some(code) => format!("Model list check failed (HTTP {code}); continuing with the manual model."),
        None => format!("Model list check failed ({}); continuing with the manual model.", probe_message(kind)),
    });
    let request = TurnRequest {
        protocol,
        base_url,
        api_key: api_key.map(str::to_string),
        model: model.to_string(),
        system: None,
        messages: vec![Message::user("Reply with one word.", String::new())],
        max_tokens: 1,
        probe: true,
    };
    let cancel = CancellationToken::new();
    let mut received_finish = false;
    let streamed = tokio::time::timeout(PROBE_TIMEOUT, run(client, &request, &cancel, |event| {
        if matches!(event, StreamEvent::Finish(_)) { received_finish = true; }
    })).await;
    match streamed {
        Ok(Ok(stream_status)) if received_finish => ProbeResult {
            ok: true,
            phase: ProbePhase::Stream,
            status: Some(stream_status),
            duration_ms: started.elapsed().as_millis() as u64,
            model_count: models.as_ref().map(Vec::len),
            stream_ok: true,
            error_category: None,
            message: "Streaming test succeeded.".into(),
            detail: None,
            models_warning: warning,
            models,
        },
        Ok(Ok(status)) => failed_probe(ProbePhase::Stream, started, ProbeErrorKind::IncompleteStream, Some(status), Some("Stream closed before a protocol completion event".into()), models, warning),
        Ok(Err(error)) => {
            let (kind, status, detail) = classify_error(&error);
            failed_probe(ProbePhase::Stream, started, kind, status, detail, models, warning)
        }
        Err(_) => failed_probe(ProbePhase::Stream, started, ProbeErrorKind::Timeout, None, Some("Request timed out".into()), models, warning),
    }
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
    list_models_detailed(client, protocol, base_url, api_key).await.map(|r| r.models)
}

pub async fn list_models_detailed(
    client: &reqwest::Client,
    protocol: Protocol,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<ModelsResponse, LlmError> {
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
    let v: Value = serde_json::from_str(&body).map_err(|e| LlmError::ProtocolResponse { status: status.as_u16(), message: format!("bad models response: {e}") })?;
    let items = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
        .cloned().ok_or_else(|| LlmError::ProtocolResponse { status: status.as_u16(), message: "unexpected models response format".into() })?;
    let mut ids: Vec<String> = items
        .iter()
        .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(Value::as_str).map(str::to_string))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ModelsResponse { models: ids, status: status.as_u16() })
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

    #[test]
    fn provider_base_url_validation_preserves_supported_hosts_and_paths() {
        assert_eq!(normalize_base_url(" http://localhost:8000/v1/ ").unwrap(), "http://localhost:8000/v1");
        assert_eq!(normalize_base_url("https://192.168.1.20:9000/custom/v2/").unwrap(), "https://192.168.1.20:9000/custom/v2");
        assert!(normalize_base_url("").is_err());
        assert!(normalize_base_url("provider.example/v1").is_err());
        assert!(normalize_base_url("ftp://provider.example/v1").is_err());
        assert!(normalize_base_url("https://user:secret@provider.example/v1").is_err());
        assert!(normalize_base_url("https://provider.example/v1?key=secret").is_err());
        assert!(normalize_base_url("https://provider.example/v1/chat/completions").is_err());
    }

    #[test]
    fn probe_errors_are_classified_without_exposing_provider_body() {
        let (kind, status, detail) = classify_error(&LlmError::Http { status: 401, message: "HTTP 401: sensitive-key rejected".into() });
        assert_eq!(kind, ProbeErrorKind::Authentication);
        assert_eq!(status, Some(401));
        assert_eq!(detail.as_deref(), Some("HTTP 401"));
        assert_eq!(classify_error(&LlmError::Http { status: 404, message: "HTTP 404".into() }).0, ProbeErrorKind::Endpoint);
        assert_eq!(classify_error(&LlmError::Http { status: 404, message: "HTTP 404: model not found".into() }).0, ProbeErrorKind::ModelUnavailable);
        assert_eq!(classify_error(&LlmError::ProtocolResponse { status: 200, message: "unexpected models response format".into() }).1, Some(200));
        assert_eq!(classify_error(&LlmError::Http { status: 429, message: "HTTP 429".into() }).0, ProbeErrorKind::RateLimited);
        assert_eq!(classify_error(&LlmError::Stream { status: 200, message: "Incorrect API key".into() }).0, ProbeErrorKind::Authentication);
        assert_eq!(classify_error(&LlmError::Stream { status: 200, message: "rate limit exceeded".into() }).0, ProbeErrorKind::RateLimited);
        assert_eq!(classify_error(&LlmError::Stream { status: 200, message: "model not found".into() }).0, ProbeErrorKind::ModelUnavailable);
        let (kind, status, detail) = classify_error(&LlmError::Stream { status: 200, message: "private-sensitive-token".into() });
        assert_eq!(kind, ProbeErrorKind::Provider);
        assert_eq!(status, Some(200));
        assert!(!detail.unwrap().contains("private-sensitive-token"));
    }
}
