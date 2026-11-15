//! Plain-JSON storage. Layout under the data root:
//!
//! ```text
//! settings.json
//! providers.json
//! keys.json            { "<provider_id>": "<api key>" }   (mode 0600)
//! sessions/<ulid>.json
//! ```
//!
//! Every write goes through a temp file + rename so a crash never leaves a
//! half-written session behind.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::model::*;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("invalid id: {0}")]
    InvalidId(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

pub fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    let now = time::OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    now.format(&Rfc3339).unwrap_or_default()
}

pub fn new_id() -> String {
    ulid::Ulid::generate().to_string().to_lowercase()
}

/// First line of the first user message, trimmed to a sidebar-sized title.
/// The first line of the message — preferring what was typed over a quoted
/// selection (`> …`, as the quick-input panel writes it), and stripping the
/// quote marker when the quote is all there is.
pub fn title_from(content: &str) -> String {
    let lines = content.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines
        .clone()
        .find(|l| !l.starts_with('>'))
        .or_else(|| lines.clone().next().map(|l| l.trim_start_matches('>').trim_start()))
        .filter(|l| !l.is_empty())
        .unwrap_or("New chat");
    let mut title: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        title.push('…');
    }
    title
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn write_atomic(path: &Path, bytes: &[u8], _secret: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        #[cfg(unix)]
        if _secret {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // std::fs::rename replaces existing files on Windows too (MoveFileExW).
    fs::rename(&tmp, path)?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl Store {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("sessions"))?;
        Ok(Store { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn session_path(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err(StoreError::InvalidId(id.to_string()));
        }
        Ok(self.sessions_dir().join(format!("{id}.json")))
    }

    // ---- settings -------------------------------------------------------

    pub fn settings(&self) -> Result<Settings> {
        Ok(read_json(&self.root.join("settings.json"))?.unwrap_or_default())
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let mut s = settings.clone();
        s.schema_version = SETTINGS_SCHEMA_VERSION;
        write_atomic(&self.root.join("settings.json"), &serde_json::to_vec_pretty(&s)?, false)
    }

    // ---- providers & keys ----------------------------------------------

    pub fn providers(&self) -> Result<Vec<Provider>> {
        let file: ProvidersFile = read_json(&self.root.join("providers.json"))?.unwrap_or_default();
        Ok(file.providers)
    }

    pub fn provider(&self, id: &str) -> Result<Option<Provider>> {
        Ok(self.providers()?.into_iter().find(|p| p.id == id))
    }

    pub fn save_providers(&self, providers: &[Provider]) -> Result<()> {
        let file = ProvidersFile {
            schema_version: PROVIDERS_SCHEMA_VERSION,
            providers: providers.to_vec(),
        };
        write_atomic(&self.root.join("providers.json"), &serde_json::to_vec_pretty(&file)?, false)
    }

    /// Insert or replace by id.
    pub fn upsert_provider(&self, provider: Provider) -> Result<Vec<Provider>> {
        let mut all = self.providers()?;
        match all.iter_mut().find(|p| p.id == provider.id) {
            Some(slot) => *slot = provider,
            None => all.push(provider),
        }
        self.save_providers(&all)?;
        Ok(all)
    }

    pub fn delete_provider(&self, id: &str) -> Result<Vec<Provider>> {
        let mut all = self.providers()?;
        all.retain(|p| p.id != id);
        self.save_providers(&all)?;
        let mut keys = self.keys()?;
        if keys.remove(id).is_some() {
            self.save_keys(&keys)?;
        }
        Ok(all)
    }

    fn keys(&self) -> Result<BTreeMap<String, String>> {
        Ok(read_json(&self.root.join("keys.json"))?.unwrap_or_default())
    }

    fn save_keys(&self, keys: &BTreeMap<String, String>) -> Result<()> {
        write_atomic(&self.root.join("keys.json"), &serde_json::to_vec_pretty(keys)?, true)
    }

    pub fn api_key(&self, provider_id: &str) -> Result<Option<String>> {
        Ok(self.keys()?.get(provider_id).cloned().filter(|k| !k.is_empty()))
    }

    pub fn set_api_key(&self, provider_id: &str, key: Option<&str>) -> Result<()> {
        let mut keys = self.keys()?;
        match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => {
                keys.insert(provider_id.to_string(), k.to_string());
            }
            None => {
                keys.remove(provider_id);
            }
        }
        self.save_keys(&keys)
    }

    pub fn provider_views(&self) -> Result<Vec<ProviderView>> {
        let keys = self.keys()?;
        Ok(self
            .providers()?
            .into_iter()
            .map(|p| ProviderView {
                has_key: keys.get(&p.id).map(|k| !k.is_empty()).unwrap_or(false),
                provider: p,
            })
            .collect())
    }

    // ---- sessions -------------------------------------------------------

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.sessions_dir())? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match read_json::<Session>(&path) {
                Ok(Some(s)) => out.push(SessionSummary::from(&s)),
                Ok(None) => {}
                Err(e) => log::warn!("skipping unreadable session {}: {e}", path.display()),
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| b.id.cmp(&a.id)));
        Ok(out)
    }

    pub fn session(&self, id: &str) -> Result<Session> {
        read_json(&self.session_path(id)?)?.ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    pub fn save_session(&self, session: &Session) -> Result<()> {
        let mut s = session.clone();
        s.schema_version = SESSION_SCHEMA_VERSION;
        write_atomic(&self.session_path(&s.id)?, &serde_json::to_vec_pretty(&s)?, false)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        match fs::remove_file(self.session_path(id)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn new_session(&self, provider_id: &str, model: &str, system: Option<String>) -> Session {
        let now = now_rfc3339();
        Session {
            schema_version: SESSION_SCHEMA_VERSION,
            id: new_id(),
            title: "New chat".to_string(),
            created_at: now.clone(),
            updated_at: now,
            provider_id: provider_id.to_string(),
            model: model.to_string(),
            system: system.filter(|s| !s.trim().is_empty()),
            messages: Vec::new(),
        }
    }

    /// Every session as one JSON object per line — the training-data view.
    pub fn export_jsonl(&self, path: &Path) -> Result<usize> {
        let mut lines = Vec::new();
        for summary in self.list_sessions()? {
            let s = self.session(&summary.id)?;
            lines.push(serde_json::to_string(&s)?);
        }
        let mut body = lines.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        write_atomic(path, body.as_bytes(), false)?;
        Ok(lines.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf()).unwrap();
        (dir, store)
    }

    #[test]
    fn repeated_writes_replace_existing_files() {
        let (dir, store) = store();

        let mut settings = store.settings().unwrap();
        settings.appearance = Appearance::Dark;
        store.save_settings(&settings).unwrap();
        settings.appearance = Appearance::Light;
        store.save_settings(&settings).unwrap();
        assert_eq!(store.settings().unwrap().appearance, Appearance::Light);

        let mut session = store.new_session("provider", "model", None);
        session.messages.push(Message::user("first", now_rfc3339()));
        store.save_session(&session).unwrap();
        session.messages[0] = Message::user("second", now_rfc3339());
        store.save_session(&session).unwrap();
        assert_eq!(store.session(&session.id).unwrap().messages[0].content.text(), "second");

        let mut provider = Provider {
            id: "provider".into(),
            name: "First".into(),
            protocol: Protocol::Chat,
            base_url: "https://example.com/v1".into(),
            models: vec!["model".into()],
        };
        store.upsert_provider(provider.clone()).unwrap();
        provider.name = "Second".into();
        store.upsert_provider(provider).unwrap();
        assert_eq!(store.provider("provider").unwrap().unwrap().name, "Second");

        store.set_api_key("provider", Some("first-key")).unwrap();
        store.set_api_key("provider", Some("second-key")).unwrap();
        assert_eq!(store.api_key("provider").unwrap().as_deref(), Some("second-key"));

        let export = dir.path().join("export.jsonl");
        fs::write(&export, b"old export").unwrap();
        assert_eq!(store.export_jsonl(&export).unwrap(), 1);
        assert!(!fs::read_to_string(&export).unwrap().contains("old export"));
    }

    #[test]
    fn unicode_data_directory_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("中文 data")).unwrap();
        store.set_api_key("provider", Some("first-key")).unwrap();
        store.set_api_key("provider", Some("second-key")).unwrap();
        assert_eq!(store.api_key("provider").unwrap().as_deref(), Some("second-key"));
    }

    #[test]
    fn session_round_trip_and_schema() {
        let (_dir, store) = store();
        let mut s = store.new_session("p1", "m1", Some("be brief".into()));
        s.messages.push(Message::user("hello", now_rfc3339()));
        s.messages.push(Message {
            role: Role::Assistant,
            content: "hi".into(),
            created_at: Some(now_rfc3339()),
            reasoning_content: None,
            thinking_blocks: None,
            meta: Some(TurnMeta {
                provider_id: "p1".into(),
                protocol: Protocol::Chat,
                model: "m1".into(),
                created_at: now_rfc3339(),
                latency_ms: Some(12),
                ttft_ms: Some(3),
                thinking_ms: None,
                usage: Usage { input_tokens: Some(1), output_tokens: Some(2), ..Usage::default() },
                finish_reason: Some("stop".into()),
                error: None,
            }),
        });
        s.title = title_from("hello");
        store.save_session(&s).unwrap();

        let loaded = store.session(&s.id).unwrap();
        assert_eq!(loaded, s);

        // The file is a replayable trajectory: messages are {role, content, ...}.
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(store.session_path(&s.id).unwrap()).unwrap()).unwrap();
        assert_eq!(raw["schema_version"], 1);
        assert_eq!(raw["messages"][0]["role"], "user");
        assert_eq!(raw["messages"][0]["content"], "hello");
        assert_eq!(raw["messages"][1]["meta"]["protocol"], "chat");
        assert_eq!(raw["messages"][1]["meta"]["usage"]["output_tokens"], 2);
        assert!(raw["messages"][0].get("meta").is_none());

        let list = store.list_sessions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].message_count, 2);

        store.delete_session(&s.id).unwrap();
        assert!(store.list_sessions().unwrap().is_empty());
        assert!(matches!(store.session(&s.id), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn list_is_newest_first_and_skips_junk() {
        let (_dir, store) = store();
        let mut a = store.new_session("p", "m", None);
        a.updated_at = "2026-01-01T00:00:00Z".into();
        let mut b = store.new_session("p", "m", None);
        b.updated_at = "2026-02-01T00:00:00Z".into();
        store.save_session(&a).unwrap();
        store.save_session(&b).unwrap();
        fs::write(store.sessions_dir().join("junk.json"), b"not json").unwrap();
        fs::write(store.sessions_dir().join("notes.txt"), b"ignored").unwrap();
        let ids: Vec<_> = store.list_sessions().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![b.id, a.id]);
    }

    #[test]
    fn providers_and_keys_are_separate_files() {
        let (_dir, store) = store();
        let p = Provider {
            id: "openai".into(),
            name: "OpenAI".into(),
            protocol: Protocol::Responses,
            base_url: "https://api.openai.com/v1".into(),
            models: vec!["gpt-5".into()],
        };
        store.upsert_provider(p.clone()).unwrap();
        store.set_api_key("openai", Some("sk-test")).unwrap();

        let providers_raw = fs::read_to_string(store.root().join("providers.json")).unwrap();
        assert!(!providers_raw.contains("sk-test"));
        assert!(providers_raw.contains("\"protocol\": \"responses\""));
        assert_eq!(store.api_key("openai").unwrap().as_deref(), Some("sk-test"));

        let views = store.provider_views().unwrap();
        assert!(views[0].has_key);
        let json = serde_json::to_value(&views[0]).unwrap();
        assert_eq!(json["id"], "openai");
        assert_eq!(json["has_key"], true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.root().join("keys.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        store.delete_provider("openai").unwrap();
        assert!(store.providers().unwrap().is_empty());
        assert!(store.api_key("openai").unwrap().is_none());
    }

    #[test]
    fn settings_default_and_save() {
        let (_dir, store) = store();
        let s = store.settings().unwrap();
        assert_eq!(s.max_tokens, 8192);
        assert!(s.sidebar_visible);
        let mut s2 = s.clone();
        s2.appearance = Appearance::Dark;
        store.save_settings(&s2).unwrap();
        assert_eq!(store.settings().unwrap().appearance, Appearance::Dark);
    }

    #[test]
    fn export_jsonl_has_one_session_per_line() {
        let (dir, store) = store();
        for i in 0..3 {
            let mut s = store.new_session("p", "m", None);
            s.messages.push(Message::user(format!("q{i}"), now_rfc3339()));
            store.save_session(&s).unwrap();
        }
        let out = dir.path().join("export.jsonl");
        assert_eq!(store.export_jsonl(&out).unwrap(), 3);
        let text = fs::read_to_string(&out).unwrap();
        assert_eq!(text.lines().count(), 3);
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["messages"][0]["role"], "user");
        }
    }

    #[test]
    fn titles_and_ids() {
        assert_eq!(title_from("\n\n  Hello world  \nmore"), "Hello world");
        assert_eq!(title_from(""), "New chat");
        assert_eq!(title_from("> quoted line\n> second\n\nWhat does it mean?"), "What does it mean?");
        assert_eq!(title_from("> quoted line\n> second"), "quoted line");
        assert_eq!(title_from(">"), "New chat");
        let long = "x".repeat(100);
        assert_eq!(title_from(&long).chars().count(), 61);
        assert!(valid_id(&new_id()));
        assert!(!valid_id("../etc/passwd"));
    }
}
