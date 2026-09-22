//! One "turn" = mutate the session, stream a completion, persist the result.
//! The engine owns the store and the set of in-flight turns; the UI layer
//! only forwards [`TurnEvent`]s. Text is accumulated here and written to the
//! store once, when the turn ends — never per token.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::llm::{self, LlmError, StreamEvent, TurnRequest};
use crate::model::*;
use crate::store::{now_rfc3339, title_from, Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("provider not found: {0}")]
    NoProvider(String),
    #[error("this chat is already generating")]
    Busy,
    #[error("{0}")]
    Llm(#[from] LlmError),
}

/// What the UI receives over the channel for one turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    /// The session after the user message has been persisted (or messages
    /// trimmed for regenerate/edit). Carries the id of a freshly created chat.
    Started { session: Session },
    Reasoning { session_id: String, delta: String },
    Text { session_id: String, delta: String },
    /// Terminal. `message` is the persisted assistant turn, if any content
    /// survived (cancelled turns keep their partial text); `error` is set
    /// when the provider or network failed.
    Done {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<Message>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        updated_at: String,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnKind {
    /// Append a user message. `session_id: None` starts a new chat.
    Send {
        #[serde(default)]
        session_id: Option<String>,
        provider_id: String,
        model: String,
        content: String,
        /// `data:` URLs; stored as image parts next to the text.
        #[serde(default)]
        images: Vec<String>,
    },
    /// Drop the trailing assistant reply and answer the last user message again.
    Regenerate { session_id: String },
    /// Replace the last user message and regenerate.
    Edit {
        session_id: String,
        content: String,
        #[serde(default)]
        images: Vec<String>,
    },
}

/// A chat's title from its first message; an image sent without words is "Image".
fn title_for(content: &str, images: &[String]) -> String {
    if content.trim().is_empty() && !images.is_empty() {
        return "Image".to_string();
    }
    title_from(content)
}

pub struct Engine {
    store: Store,
    client: reqwest::Client,
    active: Mutex<HashMap<String, CancellationToken>>,
}

impl Engine {
    pub fn new(store: Store) -> Self {
        Engine { store, client: llm::http_client(), active: Mutex::new(HashMap::new()) }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn is_active(&self, session_id: &str) -> bool {
        self.active.lock().unwrap().contains_key(session_id)
    }

    pub fn cancel(&self, session_id: &str) -> bool {
        match self.active.lock().unwrap().get(session_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Apply the turn's mutation to the session and persist it. Returns the
    /// session ready to be sent to the provider.
    fn prepare(&self, kind: &TurnKind) -> Result<Session, EngineError> {
        let now = now_rfc3339();
        let mut session = match kind {
            TurnKind::Send { session_id, provider_id, model, content, images } => {
                let mut s = match session_id {
                    Some(id) => self.store.session(id)?,
                    None => {
                        let settings = self.store.settings()?;
                        self.store.new_session(provider_id, model, settings.system_prompt)
                    }
                };
                s.provider_id = provider_id.clone();
                s.model = model.clone();
                // A dangling user message (previous attempt failed) is replaced, not stacked.
                if matches!(s.messages.last(), Some(m) if m.role == Role::User) {
                    s.messages.pop();
                }
                s.messages.push(Message::user(Content::with_images(content.clone(), images.clone()), now.clone()));
                if s.messages.len() == 1 || s.title == "New chat" {
                    s.title = title_for(content, images);
                }
                s
            }
            TurnKind::Regenerate { session_id } => {
                let mut s = self.store.session(session_id)?;
                while matches!(s.messages.last(), Some(m) if m.role == Role::Assistant) {
                    s.messages.pop();
                }
                s
            }
            TurnKind::Edit { session_id, content, images } => {
                let mut s = self.store.session(session_id)?;
                while matches!(s.messages.last(), Some(m) if m.role == Role::Assistant) {
                    s.messages.pop();
                }
                let next = Content::with_images(content.clone(), images.clone());
                match s.messages.last_mut() {
                    Some(m) if m.role == Role::User => {
                        m.content = next;
                        m.created_at = Some(now.clone());
                    }
                    _ => s.messages.push(Message::user(next, now.clone())),
                }
                if s.messages.len() == 1 {
                    s.title = title_for(content, images);
                }
                s
            }
        };
        session.updated_at = now;
        self.store.save_session(&session)?;
        Ok(session)
    }

    /// Run a turn to completion, reporting progress through `emit`.
    /// Errors that happen before the stream starts are returned; everything
    /// after `Started` is reported through `TurnEvent::Done`.
    pub async fn run_turn(&self, kind: TurnKind, mut emit: impl FnMut(TurnEvent)) -> Result<(), EngineError> {
        let session = self.prepare(&kind)?;
        let session_id = session.id.clone();

        let provider = self
            .store
            .provider(&session.provider_id)?
            .ok_or_else(|| EngineError::NoProvider(session.provider_id.clone()))?;
        let api_key = self.store.api_key(&provider.id)?;
        let settings = self.store.settings()?;

        let token = CancellationToken::new();
        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(&session_id) {
                return Err(EngineError::Busy);
            }
            active.insert(session_id.clone(), token.clone());
        }
        let _guard = ActiveGuard { engine: self, id: session_id.clone() };

        emit(TurnEvent::Started { session: session.clone() });

        let request = TurnRequest {
            protocol: provider.protocol,
            base_url: provider.base_url.clone(),
            api_key,
            model: session.model.clone(),
            system: session.system.clone(),
            messages: session.messages.clone(),
            max_tokens: settings.max_tokens,
            probe: false,
        };

        let started = Instant::now();
        let mut acc = Accumulator::default();
        let result = llm::run(&self.client, &request, &token, |ev| {
            match &ev {
                StreamEvent::Reasoning(d) => {
                    acc.first_token(started);
                    acc.first_reasoning.get_or_insert_with(Instant::now);
                    acc.reasoning.push_str(d);
                    emit(TurnEvent::Reasoning { session_id: session_id.clone(), delta: d.clone() });
                }
                StreamEvent::Text(d) => {
                    acc.first_token(started);
                    acc.first_text.get_or_insert_with(Instant::now);
                    acc.text.push_str(d);
                    emit(TurnEvent::Text { session_id: session_id.clone(), delta: d.clone() });
                }
                StreamEvent::Usage(u) => acc.merge_usage(u),
                StreamEvent::Finish(r) => acc.finish_reason = r.clone(),
            }
        })
        .await;

        let latency_ms = started.elapsed().as_millis() as u64;
        let thinking_ms = acc.first_reasoning.map(|r| acc.first_text.unwrap_or_else(Instant::now).duration_since(r).as_millis() as u64);
        let (finish_reason, error) = match &result {
            Ok(_) => (acc.finish_reason.take().or_else(|| Some("stop".into())), None),
            Err(LlmError::Cancelled) => (Some("cancelled".into()), None),
            Err(e) => (Some("error".into()), Some(e.to_string())),
        };

        let has_content = !acc.text.is_empty() || !acc.reasoning.is_empty();
        let message = if has_content {
            Some(Message {
                role: Role::Assistant,
                content: Content::Text(std::mem::take(&mut acc.text)),
                created_at: Some(now_rfc3339()),
                reasoning_content: Some(std::mem::take(&mut acc.reasoning)).filter(|r| !r.trim().is_empty()),
                meta: Some(TurnMeta {
                    provider_id: provider.id.clone(),
                    protocol: provider.protocol,
                    model: session.model.clone(),
                    created_at: now_rfc3339(),
                    latency_ms: Some(latency_ms),
                    ttft_ms: acc.ttft_ms,
                    thinking_ms,
                    usage: acc.usage,
                    finish_reason,
                    error: error.clone(),
                }),
            })
        } else {
            None
        };

        // Re-read: the user may have renamed the chat while we streamed.
        let mut fresh = self.store.session(&session_id).unwrap_or(session);
        if let Some(m) = &message {
            fresh.messages.push(m.clone());
        }
        fresh.updated_at = now_rfc3339();
        let persist = self.store.save_session(&fresh);
        let error = match (error, persist) {
            (e, Ok(())) => e,
            (Some(e), Err(p)) => Some(format!("{e} (and saving failed: {p})")),
            (None, Err(p)) => Some(format!("saving failed: {p}")),
        };

        emit(TurnEvent::Done { session_id, message, error, updated_at: fresh.updated_at });
        Ok(())
    }
}

struct ActiveGuard<'a> {
    engine: &'a Engine,
    id: String,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.engine.active.lock().unwrap().remove(&self.id);
    }
}

#[derive(Default)]
struct Accumulator {
    text: String,
    reasoning: String,
    usage: Usage,
    finish_reason: Option<String>,
    ttft_ms: Option<u64>,
    first_reasoning: Option<Instant>,
    first_text: Option<Instant>,
}

impl Accumulator {
    fn first_token(&mut self, started: Instant) {
        if self.ttft_ms.is_none() {
            self.ttft_ms = Some(started.elapsed().as_millis() as u64);
        }
    }

    /// Later usage reports refine earlier ones field by field (Anthropic sends
    /// input tokens at start and output tokens at the end).
    fn merge_usage(&mut self, u: &Usage) {
        if u.input_tokens.is_some() {
            self.usage.input_tokens = u.input_tokens;
        }
        if u.cached_input_tokens.is_some() {
            self.usage.cached_input_tokens = u.cached_input_tokens;
        }
        if u.output_tokens.is_some() {
            self.usage.output_tokens = u.output_tokens;
        }
        if u.reasoning_tokens.is_some() {
            self.usage.reasoning_tokens = u.reasoning_tokens;
        }
    }
}
