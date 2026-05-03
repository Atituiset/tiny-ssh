//! Command history backed by SQLite + Fish-style autosuggest.
//!
//! Synchronous API — SQLite operations are short-lived (microseconds) so we
//! don't bother wrapping in `spawn_blocking`. Callers that care can do so.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;
use tracing::debug;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS history (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    host         TEXT    NOT NULL,
    user         TEXT    NOT NULL,
    cwd          TEXT,
    command      TEXT    NOT NULL,
    timestamp    INTEGER NOT NULL,
    exit_code    INTEGER,
    duration_ms  INTEGER,
    source       TEXT    NOT NULL DEFAULT 'user'
);

CREATE INDEX IF NOT EXISTS idx_history_host_ts        ON history (host, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_history_host_cmd       ON history (host, command);
CREATE INDEX IF NOT EXISTS idx_history_host_cwd_cmd   ON history (host, cwd, command);
"#;

/// Persistent command history.
pub struct History {
    conn: Connection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryId(pub i64);

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: Option<HistoryId>,
    pub host: String,
    pub user: String,
    pub cwd: Option<String>,
    pub command: String,
    /// Unix epoch seconds.
    pub timestamp: i64,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub source: HistorySource,
}

/// Where a record came from. Tracks Suggestion Engine layer for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySource {
    /// User typed it themselves.
    User,
    /// User accepted a Layer-2 history suggestion.
    SuggestHistory,
    /// User accepted a Layer-3 knowledge-base suggestion.
    SuggestKnowledge,
    /// User accepted a Layer-4 LLM suggestion.
    SuggestLlm,
}

impl HistorySource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::SuggestHistory => "suggest_history",
            Self::SuggestKnowledge => "suggest_knowledge",
            Self::SuggestLlm => "suggest_llm",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "suggest_history" => Self::SuggestHistory,
            "suggest_knowledge" => Self::SuggestKnowledge,
            "suggest_llm" => Self::SuggestLlm,
            _ => Self::User,
        }
    }
}

/// Inputs for the suggestion engine.
#[derive(Debug, Clone)]
pub struct SuggestContext<'a> {
    pub host: &'a str,
    pub cwd: Option<&'a str>,
    pub prefix: &'a str,
}

/// A single autosuggest candidate.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub command: String,
    /// Where in `history` the suggestion came from. Useful for telemetry.
    pub source_id: HistoryId,
    /// Score in [0, 1] — higher is better.
    pub score: f64,
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("could not determine a default data directory")]
    NoDataDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("system clock before epoch")]
    BadClock,
}

impl History {
    /// Open the user-default history database, creating it if needed.
    pub fn open_default() -> Result<Self, HistoryError> {
        let path = default_db_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::open(path)
    }

    /// Open a history database at a specific path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, HistoryError> {
        let path = path.as_ref();
        debug!(target: "tiny_ssh::history", path = %path.display(), "opening db");
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Open an in-memory database. Useful for tests.
    pub fn open_in_memory() -> Result<Self, HistoryError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Append a new history entry.
    pub fn record(&self, mut entry: HistoryEntry) -> Result<HistoryId, HistoryError> {
        if entry.timestamp == 0 {
            entry.timestamp = now_unix_secs()?;
        }
        self.conn.execute(
            "INSERT INTO history (host, user, cwd, command, timestamp, exit_code, duration_ms, source)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                entry.host,
                entry.user,
                entry.cwd,
                entry.command,
                entry.timestamp,
                entry.exit_code,
                entry.duration_ms.map(|d| d as i64),
                entry.source.as_str(),
            ],
        )?;
        Ok(HistoryId(self.conn.last_insert_rowid()))
    }

    /// Set the exit code for a previously-recorded command.
    pub fn set_exit_code(&self, id: HistoryId, exit_code: i32) -> Result<(), HistoryError> {
        self.conn.execute(
            "UPDATE history SET exit_code = ? WHERE id = ?",
            params![exit_code, id.0],
        )?;
        Ok(())
    }

    /// Best autosuggestion for the given context. Fish-style: most recent,
    /// frequency-weighted, prefer same cwd, fall back to host-only.
    pub fn suggest(&self, ctx: &SuggestContext<'_>) -> Result<Option<Suggestion>, HistoryError> {
        if ctx.prefix.is_empty() {
            return Ok(None);
        }
        let prefix_pat = like_escape(ctx.prefix);

        // Pass 1: same host + cwd
        if let Some(cwd) = ctx.cwd {
            let row = self
                .conn
                .query_row(
                    r#"
                    SELECT id, command, COUNT(*) AS freq, MAX(timestamp) AS recency
                    FROM history
                    WHERE host = ?
                      AND cwd  = ?
                      AND command LIKE ? ESCAPE '\'
                      AND command != ?
                    GROUP BY command
                    ORDER BY recency DESC
                    LIMIT 1
                    "#,
                    params![ctx.host, cwd, format!("{prefix_pat}%"), ctx.prefix],
                    |row| {
                        let id: i64 = row.get(0)?;
                        let cmd: String = row.get(1)?;
                        let freq: i64 = row.get(2)?;
                        Ok((HistoryId(id), cmd, freq))
                    },
                )
                .optional()?;
            if let Some((id, cmd, freq)) = row {
                return Ok(Some(Suggestion {
                    command: cmd,
                    source_id: id,
                    score: score(freq, true),
                }));
            }
        }

        // Pass 2: same host, any cwd
        let row = self
            .conn
            .query_row(
                r#"
                SELECT id, command, COUNT(*) AS freq, MAX(timestamp) AS recency
                FROM history
                WHERE host = ?
                  AND command LIKE ? ESCAPE '\'
                  AND command != ?
                GROUP BY command
                ORDER BY recency DESC
                LIMIT 1
                "#,
                params![ctx.host, format!("{prefix_pat}%"), ctx.prefix],
                |row| {
                    let id: i64 = row.get(0)?;
                    let cmd: String = row.get(1)?;
                    let freq: i64 = row.get(2)?;
                    Ok((HistoryId(id), cmd, freq))
                },
            )
            .optional()?;
        Ok(row.map(|(id, cmd, freq)| Suggestion {
            command: cmd,
            source_id: id,
            score: score(freq, false),
        }))
    }

    /// Most recent N entries on this host (newest first).
    pub fn recent(&self, host: &str, limit: usize) -> Result<Vec<HistoryEntry>, HistoryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, host, user, cwd, command, timestamp, exit_code, duration_ms, source
             FROM history WHERE host = ? ORDER BY timestamp DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![host, limit as i64], row_to_entry)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let id: i64 = row.get(0)?;
    let source: String = row.get(8)?;
    Ok(HistoryEntry {
        id: Some(HistoryId(id)),
        host: row.get(1)?,
        user: row.get(2)?,
        cwd: row.get(3)?,
        command: row.get(4)?,
        timestamp: row.get(5)?,
        exit_code: row.get(6)?,
        duration_ms: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        source: HistorySource::parse(&source),
    })
}

fn score(freq: i64, same_cwd: bool) -> f64 {
    // saturating logistic: 1 use → 0.5, 5 uses → ~0.83, 10 uses → ~0.91
    let base = (freq as f64) / ((freq as f64) + 1.0);
    if same_cwd {
        (base * 0.5 + 0.5).min(1.0) // boost: floor 0.5, cap 1.0
    } else {
        base.max(0.3) // floor 0.3 so we always have *some* suggestion
    }
}

fn like_escape(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len());
    for ch in prefix.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

fn default_db_path() -> Result<PathBuf, HistoryError> {
    let dirs = ProjectDirs::from("io", "tinyssh", "tssh").ok_or(HistoryError::NoDataDir)?;
    Ok(dirs.data_dir().join("history.sqlite"))
}

fn now_unix_secs() -> Result<i64, HistoryError> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HistoryError::BadClock)?
        .as_secs();
    Ok(secs as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(host: &str, cwd: Option<&str>, cmd: &str, ts: i64) -> HistoryEntry {
        HistoryEntry {
            id: None,
            host: host.to_string(),
            user: "u".to_string(),
            cwd: cwd.map(str::to_string),
            command: cmd.to_string(),
            timestamp: ts,
            exit_code: Some(0),
            duration_ms: None,
            source: HistorySource::User,
        }
    }

    #[test]
    fn record_and_recent() {
        let h = History::open_in_memory().unwrap();
        h.record(entry("a", None, "ls", 10)).unwrap();
        h.record(entry("a", None, "pwd", 20)).unwrap();
        h.record(entry("b", None, "uname", 30)).unwrap();

        let recent_a = h.recent("a", 10).unwrap();
        assert_eq!(recent_a.len(), 2);
        assert_eq!(recent_a[0].command, "pwd");
        assert_eq!(recent_a[1].command, "ls");
    }

    #[test]
    fn suggest_prefers_recent() {
        let h = History::open_in_memory().unwrap();
        h.record(entry("h1", None, "git status", 100)).unwrap();
        h.record(entry("h1", None, "git diff", 200)).unwrap();
        h.record(entry("h1", None, "git status", 300)).unwrap();

        let s = h
            .suggest(&SuggestContext {
                host: "h1",
                cwd: None,
                prefix: "git ",
            })
            .unwrap()
            .expect("suggestion");
        assert_eq!(s.command, "git status"); // most recent matching
    }

    #[test]
    fn suggest_prefers_same_cwd() {
        let h = History::open_in_memory().unwrap();
        h.record(entry("h1", Some("/a"), "make build", 100)).unwrap();
        h.record(entry("h1", Some("/b"), "make test", 200)).unwrap();

        let s = h
            .suggest(&SuggestContext {
                host: "h1",
                cwd: Some("/a"),
                prefix: "make ",
            })
            .unwrap()
            .expect("suggestion");
        assert_eq!(s.command, "make build");
    }

    #[test]
    fn suggest_skips_exact_match() {
        let h = History::open_in_memory().unwrap();
        h.record(entry("h1", None, "ls", 100)).unwrap();

        let s = h
            .suggest(&SuggestContext {
                host: "h1",
                cwd: None,
                prefix: "ls",
            })
            .unwrap();
        assert!(s.is_none()); // nothing longer to suggest
    }

    #[test]
    fn suggest_returns_none_when_empty_prefix() {
        let h = History::open_in_memory().unwrap();
        h.record(entry("h1", None, "ls", 100)).unwrap();
        let s = h
            .suggest(&SuggestContext {
                host: "h1",
                cwd: None,
                prefix: "",
            })
            .unwrap();
        assert!(s.is_none());
    }

    #[test]
    fn like_escape_handles_wildcards() {
        let h = History::open_in_memory().unwrap();
        h.record(entry("h1", None, "echo 100%", 100)).unwrap();
        h.record(entry("h1", None, "echo other", 200)).unwrap();
        let s = h
            .suggest(&SuggestContext {
                host: "h1",
                cwd: None,
                prefix: "echo 100%",
            })
            .unwrap();
        assert!(s.is_none(), "exact prefix shouldn't suggest itself");
    }
}
