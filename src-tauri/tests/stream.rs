//! End-to-end: Engine → reqwest → a local SSE server, for all three protocols,
//! plus HTTP errors and mid-stream cancellation. No Tauri involved.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use im_lib::engine::{Engine, TurnEvent, TurnKind};
use im_lib::model::{Content, Protocol, Provider, Role};
use im_lib::store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal HTTP/1.1 server: one response per connection, chosen by path.
/// Bodies are close-delimited so chunks reach the client as they are written.
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let (head_end, body_len) = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                            .unwrap_or(0);
                        break (pos + 4, len);
                    }
                };
                while buf.len() < head_end + body_len {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                let path = head.lines().next().unwrap().split(' ').nth(1).unwrap().to_string();
                respond(&mut sock, &path, &head, &body).await;
            });
        }
    });
    format!("http://{addr}")
}

async fn write_sse(sock: &mut tokio::net::TcpStream, events: &[&str]) {
    sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
    for ev in events {
        sock.write_all(ev.as_bytes()).await.unwrap();
        sock.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let _ = sock.shutdown().await;
}

async fn respond(sock: &mut tokio::net::TcpStream, path: &str, head: &str, body: &str) {
    match path {
        "/v1/chat/completions" => {
            assert!(head.to_ascii_lowercase().contains("authorization: bearer key-1"));
            assert!(body.contains("\"stream\":true"));
            write_sse(
                sock,
                &[
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"think\"}}]}\n\n",
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n",
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n\n",
                    "data: [DONE]\n\n",
                ],
            )
            .await;
        }
        "/v1/messages" => {
            let lower = head.to_ascii_lowercase();
            assert!(lower.contains("x-api-key: key-1"), "head was: {head}");
            assert!(lower.contains("anthropic-version: 2023-06-01"));
            assert!(body.contains("\"max_tokens\":8192"));
            assert!(body.contains("\"system\":\"be terse\""));
            // A follow-up turn sends the earlier reply back with its signed thinking.
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            if let Some(prev) = v["messages"].as_array().unwrap().iter().find(|m| m["role"] == "assistant") {
                assert_eq!(
                    prev["content"],
                    serde_json::json!([
                        { "type": "thinking", "thinking": "hmm", "signature": "sig1" },
                        { "type": "redacted_thinking", "data": "opaque" },
                        { "type": "text", "text": "Hi there" },
                    ]),
                    "body was: {body}"
                );
            }
            write_sse(
                sock,
                &[
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":1}}}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig1\"}}\n\n",
                    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque\"}}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi \"}}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"there\"}}\n\n",
                    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                ],
            )
            .await;
        }
        "/v1/responses" => {
            assert!(body.contains("\"input\":[{"));
            write_sse(
                sock,
                &[
                    "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{}}\n\n",
                    "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Yo\"}\n\n",
                    "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":1,\"output_tokens_details\":{\"reasoning_tokens\":0}}}}\n\n",
                ],
            )
            .await;
        }
        // One image turn per protocol: the same stored part, three wire shapes.
        "/img/chat/completions" => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            let user = &v["messages"][1]; // after the system message
            assert_eq!(user["content"][0], serde_json::json!({ "type": "text", "text": "see" }), "body was: {body}");
            assert_eq!(user["content"][1], serde_json::json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }));
            write_sse(sock, &["data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n", "data: [DONE]\n\n"]).await;
        }
        "/img/messages" => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            let user = &v["messages"][0];
            assert_eq!(user["content"][0], serde_json::json!({ "type": "text", "text": "see" }), "body was: {body}");
            assert_eq!(user["content"][1], serde_json::json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }));
            write_sse(
                sock,
                &[
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
                    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                ],
            )
            .await;
        }
        "/img/responses" => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            let user = &v["input"][0];
            assert_eq!(user["content"][0], serde_json::json!({ "type": "input_text", "text": "see" }), "body was: {body}");
            assert_eq!(user["content"][1], serde_json::json!({ "type": "input_image", "image_url": "data:image/png;base64,AAAA" }));
            write_sse(
                sock,
                &[
                    "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
                    "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
                ],
            )
            .await;
        }
        "/bad/chat/completions" => {
            let body = r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#;
            sock.write_all(format!("HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
                .await
                .unwrap();
            let _ = sock.shutdown().await;
        }
        "/slow/chat/completions" => {
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
            sock.write_all(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n").await.unwrap();
            sock.flush().await.unwrap();
            // Then hang: the client must cancel.
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        "/json/chat/completions" => {
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"whole"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
            sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
                .await
                .unwrap();
            let _ = sock.shutdown().await;
        }
        "/v1/models" => {
            let body = r#"{"object":"list","data":[{"id":"b-model"},{"id":"a-model"}]}"#;
            sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
                .await
                .unwrap();
            let _ = sock.shutdown().await;
        }
        _ => {
            sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        }
    }
}

fn engine_with(base: &str, protocol: Protocol, prefix: &str) -> (tempfile::TempDir, Arc<Engine>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf()).unwrap();
    store
        .upsert_provider(Provider {
            id: "p".into(),
            name: "P".into(),
            protocol,
            base_url: format!("{base}/{prefix}"),
            models: vec!["m".into()],
        })
        .unwrap();
    store.set_api_key("p", Some("key-1")).unwrap();
    let mut settings = store.settings().unwrap();
    settings.system_prompt = Some("be terse".into());
    store.save_settings(&settings).unwrap();
    (dir, Arc::new(Engine::new(store)))
}

fn collect() -> (Arc<Mutex<Vec<TurnEvent>>>, impl FnMut(TurnEvent)) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    (events, move |e| sink.lock().unwrap().push(e))
}

fn send(content: &str) -> TurnKind {
    TurnKind::Send { session_id: None, provider_id: "p".into(), model: "m".into(), content: content.into(), images: vec![] }
}

#[tokio::test]
async fn chat_protocol_round_trip_persists_trajectory() {
    let base = serve().await;
    let (_dir, engine) = engine_with(&base, Protocol::Chat, "v1");
    let (events, emit) = collect();
    engine.run_turn(send("hello"), emit).await.unwrap();

    let events = events.lock().unwrap().clone();
    let TurnEvent::Started { session } = &events[0] else { panic!("expected Started, got {:?}", events[0]) };
    assert_eq!(session.messages.len(), 1);
    assert_eq!(session.title, "hello");
    assert_eq!(session.system.as_deref(), Some("be terse"));

    let text: String = events.iter().filter_map(|e| if let TurnEvent::Text { delta, .. } = e { Some(delta.as_str()) } else { None }).collect();
    assert_eq!(text, "Hello");
    let reasoning: String = events.iter().filter_map(|e| if let TurnEvent::Reasoning { delta, .. } = e { Some(delta.as_str()) } else { None }).collect();
    assert_eq!(reasoning, "think");

    let TurnEvent::Done { message, error, .. } = events.last().unwrap() else { panic!() };
    assert!(error.is_none());
    let m = message.as_ref().unwrap();
    assert_eq!(m.role, Role::Assistant);
    assert_eq!(m.content.text(), "Hello");
    assert_eq!(m.reasoning_content.as_deref(), Some("think"));
    let meta = m.meta.as_ref().unwrap();
    assert_eq!(meta.finish_reason.as_deref(), Some("stop"));
    assert_eq!(meta.usage.input_tokens, Some(12));
    assert_eq!(meta.usage.output_tokens, Some(3));
    assert!(meta.ttft_ms.is_some() && meta.latency_ms.is_some());

    // On disk: a replayable two-message trajectory.
    let saved = engine.store().session(&session.id).unwrap();
    assert_eq!(saved.messages.len(), 2);
    assert_eq!(saved.messages[1].content.text(), "Hello");
    assert!(!engine.is_active(&session.id));

    // Second turn on the same session appends; the request carries both prior messages.
    let (events2, emit2) = collect();
    engine
        .run_turn(TurnKind::Send { session_id: Some(session.id.clone()), provider_id: "p".into(), model: "m".into(), content: "again".into(), images: vec![] }, emit2)
        .await
        .unwrap();
    let TurnEvent::Started { session: s2 } = &events2.lock().unwrap()[0] else { panic!() };
    assert_eq!(s2.messages.len(), 3);
    assert_eq!(engine.store().session(&session.id).unwrap().messages.len(), 4);
}

#[tokio::test]
async fn anthropic_and_responses_protocols() {
    let base = serve().await;

    let (_d1, engine) = engine_with(&base, Protocol::Anthropic, "v1");
    let (events, emit) = collect();
    engine.run_turn(send("hi"), emit).await.unwrap();
    let TurnEvent::Done { message, error, .. } = events.lock().unwrap().last().unwrap().clone() else { panic!() };
    assert!(error.is_none());
    let m = message.unwrap();
    assert_eq!(m.content.text(), "Hi there");
    assert_eq!(m.reasoning_content.as_deref(), Some("hmm"));
    assert_eq!(
        m.thinking_blocks.as_deref().unwrap(),
        [serde_json::json!({ "type": "thinking", "thinking": "hmm", "signature": "sig1" }), serde_json::json!({ "type": "redacted_thinking", "data": "opaque" })]
    );
    let meta = m.meta.as_ref().unwrap();
    assert_eq!(meta.protocol, Protocol::Anthropic);
    assert_eq!(meta.finish_reason.as_deref(), Some("end_turn"));
    assert_eq!((meta.usage.input_tokens, meta.usage.output_tokens), (Some(20), Some(7)));
    // Second turn: the server asserts the reply went back with its thinking blocks.
    let TurnEvent::Started { session } = events.lock().unwrap()[0].clone() else { panic!() };
    let (events, emit) = collect();
    engine
        .run_turn(TurnKind::Send { session_id: Some(session.id.clone()), provider_id: "p".into(), model: "m".into(), content: "more".into(), images: vec![] }, emit)
        .await
        .unwrap();
    let TurnEvent::Done { error, .. } = events.lock().unwrap().last().unwrap().clone() else { panic!() };
    assert!(error.is_none(), "{error:?}");

    let (_d2, engine) = engine_with(&base, Protocol::Responses, "v1");
    let (events, emit) = collect();
    engine.run_turn(send("hi"), emit).await.unwrap();
    let TurnEvent::Done { message, error, .. } = events.lock().unwrap().last().unwrap().clone() else { panic!() };
    assert!(error.is_none());
    let m = message.unwrap();
    assert_eq!(m.content.text(), "Yo");
    assert!(m.reasoning_content.is_none());
    assert_eq!(m.meta.unwrap().usage.input_tokens, Some(5));
}

#[tokio::test]
async fn http_error_leaves_user_message_and_no_reply() {
    let base = serve().await;
    let (_dir, engine) = engine_with(&base, Protocol::Chat, "bad");
    let (events, emit) = collect();
    engine.run_turn(send("hello"), emit).await.unwrap();
    let events = events.lock().unwrap().clone();
    let TurnEvent::Started { session } = &events[0] else { panic!() };
    let TurnEvent::Done { message, error, .. } = events.last().unwrap() else { panic!() };
    assert!(message.is_none());
    assert_eq!(error.as_deref(), Some("HTTP 401: Incorrect API key provided"));
    let saved = engine.store().session(&session.id).unwrap();
    assert_eq!(saved.messages.len(), 1);
    assert_eq!(saved.messages[0].role, Role::User);

    // Retrying replaces the dangling user message instead of stacking a second one.
    let (events2, emit2) = collect();
    engine
        .run_turn(TurnKind::Send { session_id: Some(session.id.clone()), provider_id: "p".into(), model: "m".into(), content: "hello again".into(), images: vec![] }, emit2)
        .await
        .unwrap();
    let TurnEvent::Started { session: s2 } = &events2.lock().unwrap()[0] else { panic!() };
    assert_eq!(s2.messages.len(), 1);
    assert_eq!(s2.messages[0].content.text(), "hello again");
}

#[tokio::test]
async fn cancel_keeps_partial_text() {
    let base = serve().await;
    let (_dir, engine) = engine_with(&base, Protocol::Chat, "slow");
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let eng = engine.clone();
    let emit = move |e: TurnEvent| {
        if let TurnEvent::Text { session_id, .. } = &e {
            // First token arrived: stop, as the user would.
            assert!(eng.cancel(session_id));
        }
        sink.lock().unwrap().push(e);
    };
    tokio::time::timeout(Duration::from_secs(5), engine.run_turn(send("go"), emit)).await.expect("cancel must end the turn").unwrap();

    let events = events.lock().unwrap();
    let TurnEvent::Done { message, error, session_id, .. } = events.last().unwrap() else { panic!() };
    assert!(error.is_none());
    let m = message.as_ref().unwrap();
    assert_eq!(m.content.text(), "partial");
    assert_eq!(m.meta.as_ref().unwrap().finish_reason.as_deref(), Some("cancelled"));
    assert_eq!(engine.store().session(session_id).unwrap().messages.len(), 2);
    assert!(!engine.is_active(session_id));
}

#[tokio::test]
async fn non_streaming_json_reply_is_accepted() {
    let base = serve().await;
    let (_dir, engine) = engine_with(&base, Protocol::Chat, "json");
    let (events, emit) = collect();
    engine.run_turn(send("x"), emit).await.unwrap();
    let TurnEvent::Done { message, .. } = events.lock().unwrap().last().unwrap().clone() else { panic!() };
    assert_eq!(message.unwrap().content.text(), "whole");
}

#[tokio::test]
async fn regenerate_and_edit_rewrite_the_newest_exchange() {
    let base = serve().await;
    let (_dir, engine) = engine_with(&base, Protocol::Chat, "v1");
    let (events, emit) = collect();
    engine.run_turn(send("first"), emit).await.unwrap();
    let id = match &events.lock().unwrap()[0] {
        TurnEvent::Started { session } => session.id.clone(),
        _ => panic!(),
    };

    engine.run_turn(TurnKind::Regenerate { session_id: id.clone() }, |_| {}).await.unwrap();
    let s = engine.store().session(&id).unwrap();
    assert_eq!(s.messages.len(), 2, "regenerate replaces, never appends");
    assert_eq!(s.messages[0].content.text(), "first");

    engine.run_turn(TurnKind::Edit { session_id: id.clone(), content: "edited".into(), images: vec![] }, |_| {}).await.unwrap();
    let s = engine.store().session(&id).unwrap();
    assert_eq!(s.messages.len(), 2);
    assert_eq!(s.messages[0].content.text(), "edited");
    assert_eq!(s.messages[1].role, Role::Assistant);
}

#[tokio::test]
async fn busy_session_rejects_second_turn() {
    let base = serve().await;
    let (_dir, engine) = engine_with(&base, Protocol::Chat, "slow");
    let (events, emit) = collect();
    let e2 = engine.clone();
    let runner = tokio::spawn(async move { e2.run_turn(send("go"), emit).await });
    // Wait for the first delta so the session is registered as active.
    let id = loop {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let ev = events.lock().unwrap();
        if let Some(TurnEvent::Text { session_id, .. }) = ev.iter().find(|e| matches!(e, TurnEvent::Text { .. })) {
            break session_id.clone();
        }
    };
    let err = engine
        .run_turn(TurnKind::Send { session_id: Some(id.clone()), provider_id: "p".into(), model: "m".into(), content: "again".into(), images: vec![] }, |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "this chat is already generating");
    assert!(engine.cancel(&id));
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn list_models_sorted() {
    let base = serve().await;
    let client = im_lib::llm::http_client();
    let ids = im_lib::llm::list_models(&client, Protocol::Chat, &format!("{base}/v1"), Some("k")).await.unwrap();
    assert_eq!(ids, vec!["a-model", "b-model"]);
}

#[tokio::test]
async fn images_reach_every_protocol_and_are_stored_as_parts() {
    let base = serve().await;
    for protocol in [Protocol::Chat, Protocol::Anthropic, Protocol::Responses] {
        let (_dir, engine) = engine_with(&base, protocol, "img");
        let (events, emit) = collect();
        let kind = TurnKind::Send { session_id: None, provider_id: "p".into(), model: "m".into(), content: "see".into(), images: vec!["data:image/png;base64,AAAA".into()] };
        engine.run_turn(kind, emit).await.unwrap();
        let TurnEvent::Done { message, error, session_id, .. } = events.lock().unwrap().last().unwrap().clone() else { panic!() };
        assert!(error.is_none(), "{protocol:?}: {error:?}");
        assert_eq!(message.unwrap().content.text(), "ok");

        // On disk the user turn is parts in the chat-completions shape; the reply stays a string.
        let saved = engine.store().session(&session_id).unwrap();
        assert!(matches!(saved.messages[0].content, Content::Parts(_)));
        assert_eq!(saved.messages[0].content.text(), "see");
        assert_eq!(saved.messages[0].content.images(), vec!["data:image/png;base64,AAAA"]);
        assert!(matches!(saved.messages[1].content, Content::Text(_)));
        assert_eq!(saved.title, "see");
    }
}
