//! Session and message persistence.

use crate::db::Store;
use nash_core::error::{EngineError, Result};
use nash_core::message::ChatMessage;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

impl Session {
    /// A fresh session with a generated id and the given title.
    pub fn new(title: Option<String>) -> Self {
        let now = nash_core::now_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            title,
            provider: None,
            model: None,
            pinned: false,
            tags: Vec::new(),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// A persisted message with its ordering metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub session_id: String,
    /// Monotonic position within the session.
    pub seq: i64,
    /// Role as text (`system`/`user`/`assistant`/`tool`).
    pub role: String,
    pub message: ChatMessage,
    pub created_at: String,
}

impl Store {
    /// Create and persist a new session.
    pub fn create_session(&self, title: Option<String>) -> Result<Session> {
        let session = Session::new(title);
        self.upsert_session(&session)?;
        Ok(session)
    }

    /// Insert or replace a session row.
    pub fn upsert_session(&self, s: &Session) -> Result<()> {
        let tags = serde_json::to_string(&s.tags)?;
        let metadata = serde_json::to_string(&s.metadata)?;
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO sessions (id, title, provider, model, pinned, tags, metadata, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    title=?2, provider=?3, model=?4, pinned=?5, tags=?6, metadata=?7, updated_at=?9",
                params![
                    s.id,
                    s.title,
                    s.provider,
                    s.model,
                    s.pinned as i64,
                    tags,
                    metadata,
                    s.created_at,
                    s.updated_at,
                ],
            )
            .map_err(map_row_err)?;
            Ok(())
        })
    }

    /// Fetch one session.
    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT id, title, provider, model, pinned, tags, metadata, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id],
                row_to_session,
            )
            .optional()
            .map_err(map_row_err)
        })
    }

    /// List sessions, pinned first then by recency.
    pub fn list_sessions(&self, limit: i64, offset: i64) -> Result<Vec<Session>> {
        self.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, title, provider, model, pinned, tags, metadata, created_at, updated_at
                     FROM sessions ORDER BY pinned DESC, updated_at DESC LIMIT ?1 OFFSET ?2",
                )
                .map_err(map_row_err)?;
            let rows = stmt
                .query_map(params![limit.max(0), offset.max(0)], row_to_session)
                .map_err(map_row_err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_row_err)
        })
    }

    /// Rename a session.
    pub fn rename_session(&self, id: &str, title: impl Into<String>) -> Result<()> {
        let title = title.into();
        self.touch_fields(id, "title", &title)
    }

    /// Set the provider/model a session is bound to.
    pub fn set_session_model(
        &self,
        id: &str,
        provider: Option<String>,
        model: Option<String>,
    ) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE sessions SET provider=?2, model=?3, updated_at=?4 WHERE id=?1",
                params![id, provider, model, nash_core::now_rfc3339()],
            )
            .map_err(map_row_err)?;
            Ok(())
        })
    }

    /// Pin or unpin a session.
    pub fn set_session_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE sessions SET pinned=?2, updated_at=?3 WHERE id=?1",
                params![id, pinned as i64, nash_core::now_rfc3339()],
            )
            .map_err(map_row_err)?;
            Ok(())
        })
    }

    /// Bump `updated_at` (called when messages are added).
    pub fn touch_session(&self, id: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "UPDATE sessions SET updated_at=?2 WHERE id=?1",
                params![id, nash_core::now_rfc3339()],
            )
            .map_err(map_row_err)?;
            Ok(())
        })
    }

    /// Delete a session and (via cascade) its messages.
    pub fn delete_session(&self, id: &str) -> Result<bool> {
        self.with_conn(|c| {
            let n = c
                .execute("DELETE FROM sessions WHERE id=?1", params![id])
                .map_err(map_row_err)?;
            Ok(n > 0)
        })
    }

    /// Append a message to a session, assigning the next sequence number.
    pub fn add_message(&self, session_id: &str, message: &ChatMessage) -> Result<StoredMessage> {
        let body = serde_json::to_string(message)?;
        let role = role_name(message);
        let id = Uuid::new_v4().to_string();
        let now = nash_core::now_rfc3339();
        self.with_tx(|c| {
            let exists: Option<String> = c
                .query_row(
                    "SELECT id FROM sessions WHERE id=?1",
                    params![session_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_row_err)?;
            if exists.is_none() {
                return Err(EngineError::Other(format!(
                    "session '{session_id}' does not exist"
                )));
            }
            let next: i64 = c
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) + 1 FROM messages WHERE session_id=?1",
                    params![session_id],
                    |r| r.get(0),
                )
                .map_err(map_row_err)?;
            c.execute(
                "INSERT INTO messages (id, session_id, seq, role, body, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, session_id, next, role, body, now],
            )
            .map_err(map_row_err)?;
            c.execute(
                "UPDATE sessions SET updated_at=?2 WHERE id=?1",
                params![session_id, now],
            )
            .map_err(map_row_err)?;
            Ok(StoredMessage {
                id: id.clone(),
                session_id: session_id.to_string(),
                seq: next,
                role: role.to_string(),
                message: message.clone(),
                created_at: now.clone(),
            })
        })
    }

    /// All messages of a session in order.
    pub fn messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, session_id, seq, role, body, created_at
                     FROM messages WHERE session_id=?1 ORDER BY seq ASC",
                )
                .map_err(map_row_err)?;
            let rows = stmt
                .query_map(params![session_id], row_to_message)
                .map_err(map_row_err)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_row_err)
        })
    }

    /// Delete all messages in a session (keeps the session itself).
    pub fn clear_messages(&self, session_id: &str) -> Result<usize> {
        self.with_conn(|c| {
            let n = c
                .execute("DELETE FROM messages WHERE session_id=?1", params![session_id])
                .map_err(map_row_err)?;
            Ok(n)
        })
    }

    fn touch_fields(&self, id: &str, field: &str, value: &str) -> Result<()> {
        // `field` is a code-controlled literal, never user input.
        let sql = format!("UPDATE sessions SET {field}=?2, updated_at=?3 WHERE id=?1");
        self.with_conn(|c| {
            c.execute(&sql, params![id, value, nash_core::now_rfc3339()])
                .map_err(map_row_err)?;
            Ok(())
        })
    }

    /// Report the schema version this database was written with.
    pub fn schema_version(&self) -> Result<i64> {
        self.with_conn(|c| {
            c.query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(map_row_err)
        })
    }
}

fn role_name(m: &ChatMessage) -> &'static str {
    use nash_core::message::Role::*;
    match m.role {
        System => "system",
        User => "user",
        Assistant => "assistant",
        Tool => "tool",
    }
}

fn row_to_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    let tags: String = row.get(5)?;
    let metadata: String = row.get(6)?;
    Ok(Session {
        id: row.get(0)?,
        title: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        pinned: row.get::<_, i64>(4)? != 0,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        metadata: serde_json::from_str(&metadata).unwrap_or(serde_json::Value::Null),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_message(row: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    let body: String = row.get(4)?;
    let message: ChatMessage = serde_json::from_str(&body).unwrap_or_else(|_| ChatMessage::user(""));
    Ok(StoredMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        seq: row.get(2)?,
        role: row.get(3)?,
        message,
        created_at: row.get(5)?,
    })
}

fn map_row_err(e: rusqlite::Error) -> EngineError {
    EngineError::Other(format!("database error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nash_core::message::ChatMessage;

    #[test]
    fn create_list_and_delete_session() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_session(Some("first".into())).unwrap();
        let _second = store.create_session(None).unwrap();
        assert_eq!(store.list_sessions(10, 0).unwrap().len(), 2);
        assert!(store.get_session(&a.id).unwrap().is_some());

        store.set_session_pinned(&a.id, true).unwrap();
        let listed = store.list_sessions(10, 0).unwrap();
        assert!(listed[0].pinned);

        assert!(store.delete_session(&a.id).unwrap());
        assert!(store.get_session(&a.id).unwrap().is_none());
        assert_eq!(store.list_sessions(10, 0).unwrap().len(), 1);
    }

    #[test]
    fn messages_are_ordered_and_cascade() {
        let store = Store::open_in_memory().unwrap();
        let s = store.create_session(None).unwrap();
        store.add_message(&s.id, &ChatMessage::user("one")).unwrap();
        store
            .add_message(&s.id, &ChatMessage::assistant("two"))
            .unwrap();
        let msgs = store.messages(&s.id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].seq, 0);
        assert_eq!(msgs[1].seq, 1);
        assert_eq!(msgs[0].message.text(), "one");
        assert_eq!(msgs[1].role, "assistant");

        assert!(store.delete_session(&s.id).unwrap());
        assert!(store.messages(&s.id).unwrap().is_empty());
    }

    #[test]
    fn adding_to_missing_session_errors() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.add_message("ghost", &ChatMessage::user("x")).is_err());
    }

    #[test]
    fn rename_and_pin() {
        let store = Store::open_in_memory().unwrap();
        let s = store.create_session(Some("old".into())).unwrap();
        store.rename_session(&s.id, "new").unwrap();
        store
            .set_session_model(&s.id, Some("openai".into()), Some("gpt-4o".into()))
            .unwrap();
        let got = store.get_session(&s.id).unwrap().unwrap();
        assert_eq!(got.title.as_deref(), Some("new"));
        assert_eq!(got.provider.as_deref(), Some("openai"));
        assert_eq!(got.model.as_deref(), Some("gpt-4o"));
    }
}
