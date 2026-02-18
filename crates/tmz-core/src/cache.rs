//! `SQLite` cache for Teams conversations and messages.
//!
//! Stores synced data locally for fast searching and offline access.
//! The database lives at `$XDG_DATA_HOME/tmz/cache.db`.

use crate::CoreError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::str::FromStr;

/// `SQLite` cache database.
#[derive(Debug, Clone)]
pub struct Cache {
    pool: SqlitePool,
}

/// A cached conversation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedConversation {
    /// Conversation thread ID.
    pub id: String,
    /// Display name / topic.
    pub display_name: String,
    /// Thread type (chat, topic, space, etc.).
    pub thread_type: String,
    /// Product type (`OneToOneChat`, `GroupChat`, `TeamsStandardChannel`, etc.).
    pub product_type: String,
    /// Last message preview.
    pub last_message_preview: String,
    /// Last message sender display name.
    pub last_message_from: String,
    /// Last activity time (ISO 8601).
    pub last_activity: String,
    /// Messages URL for fetching messages.
    pub messages_url: String,
    /// Comma-separated member display names.
    pub member_names: String,
    /// Raw JSON from the API (for --json output).
    pub raw_json: String,
}

/// A cached message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedMessage {
    /// Message ID.
    pub id: String,
    /// Conversation thread ID.
    pub conversation_id: String,
    /// Sender display name.
    pub from_display_name: String,
    /// Message content (HTML stripped to plain text for display).
    pub content: String,
    /// Raw HTML content.
    pub content_html: String,
    /// Message type (RichText/Html, Text, etc.).
    pub message_type: String,
    /// Compose time (ISO 8601).
    pub compose_time: String,
    /// Whether the message is from the current user.
    pub is_from_me: bool,
    /// Raw JSON from the API.
    pub raw_json: String,
}

/// Search result combining message with conversation context.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    /// The matched message.
    pub message: CachedMessage,
    /// Display name of the conversation.
    pub conversation_name: String,
}

impl Cache {
    /// Open or create the cache database at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened or migrations fail.
    pub async fn open(db_path: &Path) -> Result<Self, CoreError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(CoreError::Io)?;
        }

        let db_url = format!("sqlite:{}", db_path.display());
        let options = SqliteConnectOptions::from_str(&db_url)
            .map_err(|e| CoreError::Other(format!("invalid db path: {e}")))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(|e| CoreError::Other(format!("opening cache db: {e}")))?;

        let cache = Self { pool };
        cache.run_migrations().await?;
        Ok(cache)
    }

    async fn run_migrations(&self) -> Result<(), CoreError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL DEFAULT '',
                thread_type TEXT NOT NULL DEFAULT '',
                product_type TEXT NOT NULL DEFAULT '',
                last_message_preview TEXT NOT NULL DEFAULT '',
                last_message_from TEXT NOT NULL DEFAULT '',
                last_activity TEXT NOT NULL DEFAULT '',
                messages_url TEXT NOT NULL DEFAULT '',
                member_names TEXT NOT NULL DEFAULT '',
                raw_json TEXT NOT NULL DEFAULT '{}'
            )"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("creating conversations table: {e}")))?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                id TEXT NOT NULL,
                conversation_id TEXT NOT NULL,
                from_display_name TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL DEFAULT '',
                content_html TEXT NOT NULL DEFAULT '',
                message_type TEXT NOT NULL DEFAULT '',
                compose_time TEXT NOT NULL DEFAULT '',
                is_from_me INTEGER NOT NULL DEFAULT 0,
                raw_json TEXT NOT NULL DEFAULT '{}',
                PRIMARY KEY (id, conversation_id)
            )"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("creating messages table: {e}")))?;

        // FTS5 virtual table for full-text search across messages
        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content, from_display_name, conversation_id,
                content=messages,
                content_rowid=rowid
            )"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("creating FTS table: {e}")))?;

        // Triggers to keep FTS in sync
        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content, from_display_name, conversation_id)
                VALUES (new.rowid, new.content, new.from_display_name, new.conversation_id);
            END"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("creating FTS insert trigger: {e}")))?;

        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, from_display_name, conversation_id)
                VALUES ('delete', old.rowid, old.content, old.from_display_name, old.conversation_id);
            END"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("creating FTS delete trigger: {e}")))?;

        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, from_display_name, conversation_id)
                VALUES ('delete', old.rowid, old.content, old.from_display_name, old.conversation_id);
                INSERT INTO messages_fts(rowid, content, from_display_name, conversation_id)
                VALUES (new.rowid, new.content, new.from_display_name, new.conversation_id);
            END"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("creating FTS update trigger: {e}")))?;

        // Index for fast conversation lookups
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_messages_conversation
             ON messages(conversation_id, compose_time DESC)"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("creating message index: {e}")))?;

        // Index for conversation display name search
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_conversations_name
             ON conversations(display_name COLLATE NOCASE)"
        )
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("creating conversation name index: {e}")))?;

        Ok(())
    }

    /// Upsert a conversation into the cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the database write fails.
    pub async fn upsert_conversation(&self, conv: &CachedConversation) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO conversations (id, display_name, thread_type, product_type,
             last_message_preview, last_message_from, last_activity, messages_url,
             member_names, raw_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                display_name = excluded.display_name,
                thread_type = excluded.thread_type,
                product_type = excluded.product_type,
                last_message_preview = excluded.last_message_preview,
                last_message_from = excluded.last_message_from,
                last_activity = excluded.last_activity,
                messages_url = excluded.messages_url,
                member_names = excluded.member_names,
                raw_json = excluded.raw_json"
        )
        .bind(&conv.id)
        .bind(&conv.display_name)
        .bind(&conv.thread_type)
        .bind(&conv.product_type)
        .bind(&conv.last_message_preview)
        .bind(&conv.last_message_from)
        .bind(&conv.last_activity)
        .bind(&conv.messages_url)
        .bind(&conv.member_names)
        .bind(&conv.raw_json)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("upserting conversation: {e}")))?;

        Ok(())
    }

    /// Upsert a message into the cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the database write fails.
    pub async fn upsert_message(&self, msg: &CachedMessage) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO messages (id, conversation_id, from_display_name, content,
             content_html, message_type, compose_time, is_from_me, raw_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id, conversation_id) DO UPDATE SET
                from_display_name = excluded.from_display_name,
                content = excluded.content,
                content_html = excluded.content_html,
                message_type = excluded.message_type,
                compose_time = excluded.compose_time,
                is_from_me = excluded.is_from_me,
                raw_json = excluded.raw_json"
        )
        .bind(&msg.id)
        .bind(&msg.conversation_id)
        .bind(&msg.from_display_name)
        .bind(&msg.content)
        .bind(&msg.content_html)
        .bind(&msg.message_type)
        .bind(&msg.compose_time)
        .bind(msg.is_from_me)
        .bind(&msg.raw_json)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("upserting message: {e}")))?;

        Ok(())
    }

    /// List conversations, ordered by last activity.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn list_conversations(&self, limit: i64) -> Result<Vec<CachedConversation>, CoreError> {
        let rows = sqlx::query(
            "SELECT * FROM conversations ORDER BY last_activity DESC LIMIT ?"
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("listing conversations: {e}")))?;

        Ok(rows.iter().map(row_to_conversation).collect())
    }

    /// Find a conversation by fuzzy matching on display name, member names, or ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn find_conversation(&self, query: &str) -> Result<Vec<CachedConversation>, CoreError> {
        let pattern = format!("%{query}%");
        let rows = sqlx::query(
            "SELECT * FROM conversations
             WHERE display_name LIKE ?1 COLLATE NOCASE
                OR member_names LIKE ?1 COLLATE NOCASE
                OR id LIKE ?1 COLLATE NOCASE
             ORDER BY last_activity DESC
             LIMIT 10"
        )
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("finding conversation: {e}")))?;

        Ok(rows.iter().map(row_to_conversation).collect())
    }

    /// Get recent messages from a conversation.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn get_messages(
        &self,
        conversation_id: &str,
        limit: i64,
    ) -> Result<Vec<CachedMessage>, CoreError> {
        let rows = sqlx::query(
            "SELECT * FROM messages
             WHERE conversation_id = ?
             ORDER BY compose_time DESC
             LIMIT ?"
        )
        .bind(conversation_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("getting messages: {e}")))?;

        // Return in chronological order (oldest first)
        let mut msgs: Vec<CachedMessage> = rows.iter().map(row_to_message).collect();
        msgs.reverse();
        Ok(msgs)
    }

    /// Get the latest messages across the most recently active conversations.
    ///
    /// Returns messages grouped by conversation, ordered by last activity.
    /// Each conversation returns up to `per_chat` most recent messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn latest_across_chats(
        &self,
        num_chats: i64,
        per_chat: i64,
    ) -> Result<Vec<(CachedConversation, Vec<CachedMessage>)>, CoreError> {
        let convs = self.list_conversations(num_chats).await?;
        let mut result = Vec::new();
        for conv in convs {
            let msgs = self.get_messages(&conv.id, per_chat).await?;
            if !msgs.is_empty() {
                result.push((conv, msgs));
            }
        }
        Ok(result)
    }

    /// Full-text search across all cached messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchResult>, CoreError> {
        let rows = sqlx::query(
            "SELECT m.*, c.display_name AS conversation_name
             FROM messages_fts fts
             JOIN messages m ON m.rowid = fts.rowid
             LEFT JOIN conversations c ON c.id = m.conversation_id
             WHERE messages_fts MATCH ?
             ORDER BY m.compose_time DESC
             LIMIT ?"
        )
        .bind(query)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("searching messages: {e}")))?;

        Ok(rows
            .iter()
            .map(|row| SearchResult {
                message: row_to_message(row),
                conversation_name: row.get::<String, _>("conversation_name"),
            })
            .collect())
    }

    /// Full-text search within a specific conversation.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn search_in_conversation(
        &self,
        query: &str,
        conversation_id: &str,
        limit: i64,
    ) -> Result<Vec<SearchResult>, CoreError> {
        let rows = sqlx::query(
            "SELECT m.*, c.display_name AS conversation_name
             FROM messages_fts fts
             JOIN messages m ON m.rowid = fts.rowid
             LEFT JOIN conversations c ON c.id = m.conversation_id
             WHERE messages_fts MATCH ?
               AND m.conversation_id = ?
             ORDER BY m.compose_time DESC
             LIMIT ?",
        )
        .bind(query)
        .bind(conversation_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Other(format!("searching messages: {e}")))?;

        Ok(rows
            .iter()
            .map(|row| SearchResult {
                message: row_to_message(row),
                conversation_name: row.get::<String, _>("conversation_name"),
            })
            .collect())
    }

    /// Get cache statistics.
    ///
    /// # Errors
    ///
    /// Returns an error if the database read fails.
    pub async fn stats(&self) -> Result<CacheStats, CoreError> {
        let conv_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| CoreError::Other(format!("counting conversations: {e}")))?;

        let msg_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| CoreError::Other(format!("counting messages: {e}")))?;

        Ok(CacheStats {
            conversations: conv_count,
            messages: msg_count,
        })
    }
}

/// Cache statistics.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct CacheStats {
    /// Number of cached conversations.
    pub conversations: i64,
    /// Number of cached messages.
    pub messages: i64,
}

fn row_to_conversation(row: &sqlx::sqlite::SqliteRow) -> CachedConversation {
    CachedConversation {
        id: row.get("id"),
        display_name: row.get("display_name"),
        thread_type: row.get("thread_type"),
        product_type: row.get("product_type"),
        last_message_preview: row.get("last_message_preview"),
        last_message_from: row.get("last_message_from"),
        last_activity: row.get("last_activity"),
        messages_url: row.get("messages_url"),
        member_names: row.get("member_names"),
        raw_json: row.get("raw_json"),
    }
}

fn row_to_message(row: &sqlx::sqlite::SqliteRow) -> CachedMessage {
    CachedMessage {
        id: row.get("id"),
        conversation_id: row.get("conversation_id"),
        from_display_name: row.get("from_display_name"),
        content: row.get("content"),
        content_html: row.get("content_html"),
        message_type: row.get("message_type"),
        compose_time: row.get("compose_time"),
        is_from_me: row.get::<bool, _>("is_from_me"),
        raw_json: row.get("raw_json"),
    }
}

/// Convert Teams HTML message to readable plain text.
///
/// Handles block elements (`<p>`, `<br>`, `<div>`), strips quoted replies
/// (`<blockquote>`), decodes HTML entities, and collapses whitespace.
#[must_use]
pub fn strip_html(html: &str) -> String {
    // Pre-process: insert newlines for block-level elements
    let mut s = html.to_string();

    // Remove blockquote sections entirely (quoted reply context)
    while let Some(start) = s.find("<blockquote") {
        if let Some(end) = s[start..].find("</blockquote>") {
            s = format!("{}{}", &s[..start], &s[start + end + "</blockquote>".len()..]);
        } else {
            break;
        }
    }

    // Block-level tags -> newline
    for tag in &["<br>", "<br/>", "<br />", "</p>", "</div>", "</li>"] {
        s = s.replace(tag, "\n");
    }

    // Opening block tags that shouldn't add extra newlines
    for tag_prefix in &["<p", "<div", "<li"] {
        while let Some(pos) = s.find(tag_prefix) {
            if let Some(end) = s[pos..].find('>') {
                s = format!("{}{}", &s[..pos], &s[pos + end + 1..]);
            } else {
                break;
            }
        }
    }

    // Strip remaining HTML tags
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    // Decode HTML entities
    let result = result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    // Decode numeric entities (&#128077; etc.)
    let result = decode_numeric_entities(&result);

    // Clean up whitespace: collapse spaces within lines, trim blank lines
    result
        .lines()
        .map(|line| {
            // Collapse runs of spaces/tabs within each line
            let mut collapsed = String::new();
            let mut last_was_space = false;
            for ch in line.chars() {
                if ch == ' ' || ch == '\t' {
                    if !last_was_space && !collapsed.is_empty() {
                        collapsed.push(' ');
                        last_was_space = true;
                    }
                } else {
                    collapsed.push(ch);
                    last_was_space = false;
                }
            }
            collapsed.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Decode numeric HTML entities like `&#128077;` to their Unicode characters.
fn decode_numeric_entities(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '&' && chars.peek() == Some(&'#') {
            chars.next(); // consume '#'
            let mut num_str = String::new();
            for digit in chars.by_ref() {
                if digit == ';' {
                    break;
                }
                num_str.push(digit);
            }
            if let Ok(code) = num_str.parse::<u32>()
                && let Some(c) = char::from_u32(code)
            {
                result.push(c);
                continue;
            }
            // Failed to parse - put it back as-is
            result.push('&');
            result.push('#');
            result.push_str(&num_str);
            result.push(';');
        } else {
            result.push(ch);
        }
    }

    result
}

/// Parse a Teams API conversation JSON object into a `CachedConversation`.
#[must_use]
pub fn parse_conversation(conv: &serde_json::Value) -> CachedConversation {
    let id = conv["id"].as_str().unwrap_or("").to_string();
    let tp = &conv["threadProperties"];
    let lm = &conv["lastMessage"];

    let topic = tp["topic"].as_str().unwrap_or("");
    let product_type = tp["productThreadType"].as_str().unwrap_or("");
    let thread_type = tp["threadType"].as_str().unwrap_or("");

    // Build display name: use topic for channels, member names for chats
    let display_name = if topic.is_empty() {
        // For 1:1 and group chats, use the last message sender or conversation type
        let from_name = lm["imdisplayname"].as_str().unwrap_or("");
        if from_name.is_empty() {
            product_type.to_string()
        } else {
            from_name.to_string()
        }
    } else {
        topic.to_string()
    };

    let last_content = lm["content"].as_str().unwrap_or("");
    let last_preview = strip_html(last_content);
    let last_from = lm["imdisplayname"].as_str().unwrap_or("").to_string();
    let last_activity = lm["composetime"].as_str().unwrap_or("").to_string();
    let messages_url = conv["messages"].as_str().unwrap_or("").to_string();

    let raw_json =
        serde_json::to_string(conv).unwrap_or_default();

    CachedConversation {
        id,
        display_name,
        thread_type: thread_type.to_string(),
        product_type: product_type.to_string(),
        last_message_preview: last_preview,
        last_message_from: last_from,
        last_activity,
        messages_url,
        member_names: String::new(), // populated during sync if members fetched
        raw_json,
    }
}

/// Parse a Teams API message JSON object into a `CachedMessage`.
#[must_use]
pub fn parse_message(msg: &serde_json::Value, conversation_id: &str) -> Option<CachedMessage> {
    let msg_type = msg["messagetype"].as_str().unwrap_or("");

    // Skip system/control messages
    if !matches!(msg_type, "RichText/Html" | "Text" | "RichText") {
        return None;
    }

    let id = msg["id"].as_str().unwrap_or("").to_string();
    let content_html = msg["content"].as_str().unwrap_or("").to_string();
    let content = strip_html(&content_html);
    let from_name = msg["imdisplayname"].as_str().unwrap_or("").to_string();
    let compose_time = msg["composetime"].as_str().unwrap_or("").to_string();
    let is_from_me = msg["isFromMe"].as_bool().unwrap_or(false);
    let raw_json = serde_json::to_string(msg).unwrap_or_default();

    Some(CachedMessage {
        id,
        conversation_id: conversation_id.to_string(),
        from_display_name: from_name,
        content,
        content_html,
        message_type: msg_type.to_string(),
        compose_time,
        is_from_me,
        raw_json,
    })
}
