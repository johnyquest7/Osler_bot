/// Long-term persistent memory backed by SQLite with FTS5.
///
/// Stores summaries of past conversations and retrieves the most relevant ones
/// for the current session using full-text search.  Vector embeddings are
/// left as a future extension point – FTS5 gives good-enough recall for a
/// personal assistant without requiring additional native libraries.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::config::data_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: i64,
    pub timestamp: String,
    pub summary: String,
    pub content: String,
    pub tags: String,
}

pub struct LongTermMemory {
    conn: Connection,
}

impl LongTermMemory {
    pub fn open() -> Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("memory.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("Opening memory DB at {}", db_path.display()))?;
        let mem = Self { conn };
        mem.migrate()?;
        Ok(mem)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS memories (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT    NOT NULL,
                summary   TEXT    NOT NULL,
                content   TEXT    NOT NULL,
                tags      TEXT    NOT NULL DEFAULT ''
            );

            -- FTS5 table that mirrors the memories table for keyword search
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                summary,
                content,
                tags,
                content='memories',
                content_rowid='id'
            );

            -- Keep FTS in sync via triggers
            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, summary, content, tags)
                VALUES (new.id, new.summary, new.content, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, summary, content, tags)
                VALUES ('delete', old.id, old.summary, old.content, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, summary, content, tags)
                VALUES ('delete', old.id, old.summary, old.content, old.tags);
                INSERT INTO memories_fts(rowid, summary, content, tags)
                VALUES (new.id, new.summary, new.content, new.tags);
            END;
        ")?;
        Ok(())
    }

    /// Store a new memory entry.
    pub fn store(&self, summary: &str, content: &str, tags: &[&str]) -> Result<i64> {
        let tags_str = tags.join(",");
        let timestamp = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO memories (timestamp, summary, content, tags) VALUES (?1, ?2, ?3, ?4)",
            params![timestamp, summary, content, tags_str],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Full-text search for the most relevant memories given a query string.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        if query.trim().is_empty() {
            return self.recent(limit);
        }

        // Sanitise query: strip special FTS5 chars to avoid syntax errors.
        let safe_query = sanitise_fts_query(query);

        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.timestamp, m.summary, m.content, m.tags
             FROM memories_fts f
             JOIN memories m ON m.id = f.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;

        let entries = stmt
            .query_map(params![safe_query, limit as i64], row_to_entry)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Return the N most recent entries (fallback when FTS returns nothing).
    pub fn recent(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, summary, content, tags
             FROM memories
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let entries = stmt
            .query_map(params![limit as i64], row_to_entry)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn count(&self) -> Result<usize> {
        let n: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Format retrieved memories as a compact bullet list for system-prompt
    /// injection.
    pub fn format_context(entries: &[MemoryEntry]) -> String {
        if entries.is_empty() {
            return String::new();
        }
        entries
            .iter()
            .enumerate()
            .map(|(i, e)| format!("{}. [{}] {}", i + 1, e.timestamp[..10].to_string(), e.summary))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryEntry> {
    Ok(MemoryEntry {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        summary: row.get(2)?,
        content: row.get(3)?,
        tags: row.get(4)?,
    })
}

/// Remove FTS5 special characters to prevent query syntax errors.
fn sanitise_fts_query(input: &str) -> String {
    // Keep alphanumeric, spaces, and hyphens; quote the whole thing as a phrase
    let clean: String = input
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
        .collect();
    // Wrap in double-quotes for phrase-like matching
    format!("\"{}\"", clean.trim())
}
