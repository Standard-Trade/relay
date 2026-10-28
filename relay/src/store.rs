//! SQLite store: the post queue, what was sent, paused accounts, the stop flag.
//!
//! Account state for the policy is derived from the posts table on every
//! tick rather than kept as counters, so a crash or restart can't lose count.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::policy::{day_of, AccountState, Mode};

pub struct Store {
    pub conn: Connection,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Post {
    pub id: String,
    pub account: String,
    pub scheduled_at: i64,
    pub images: Vec<String>,
    pub caption: String,
    pub hook: String,
    pub status: String,
}

/// Post statuses. `queued` → `sent` → `published` | `failed`; or `dropped`.
pub const QUEUED: &str = "queued";
pub const SENT: &str = "sent";
pub const PUBLISHED: &str = "published";
pub const FAILED: &str = "failed";
pub const DROPPED: &str = "dropped";
/// Claimed and being sent right now. Counts toward the day's cap.
pub const SENDING: &str = "sending";
/// A crash left it mid-send: it may or may not be live. Needs a person.
pub const UNKNOWN: &str = "unknown";

impl Store {
    pub fn open(path: &str) -> Result<Store> {
        let conn = Connection::open(path).with_context(|| format!("opening {path}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS posts (
                id TEXT PRIMARY KEY,
                account TEXT NOT NULL,
                scheduled_at INTEGER NOT NULL,
                images TEXT NOT NULL,
                caption TEXT NOT NULL,
                hook TEXT NOT NULL,
                status TEXT NOT NULL,
                mode TEXT,
                sent_at INTEGER,
                publish_id TEXT,
                detail TEXT
             );
             CREATE INDEX IF NOT EXISTS posts_due ON posts(status, scheduled_at);
             CREATE TABLE IF NOT EXISTS accounts (
                handle TEXT PRIMARY KEY,
                paused INTEGER NOT NULL DEFAULT 0,
                started_at INTEGER,
                reason TEXT
             );
             CREATE TABLE IF NOT EXISTS engine (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS tokens (
                handle TEXT PRIMARY KEY,
                access_token TEXT NOT NULL,
                refresh_token TEXT,
                expires_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS events (
                at INTEGER NOT NULL,
                account TEXT,
                post TEXT,
                kind TEXT NOT NULL,
                detail TEXT
             );",
        )?;
        Ok(Store { conn })
    }

    pub fn enqueue(&self, post: &Post) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO posts (id, account, scheduled_at, images, caption, hook, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                post.id,
                post.account,
                post.scheduled_at,
                serde_json::to_string(&post.images)?,
                post.caption,
                post.hook,
                QUEUED
            ],
        )?;
        Ok(n == 1)
    }

    pub fn due(&self, now: i64) -> Result<Vec<Post>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account, scheduled_at, images, caption, hook, status FROM posts
             WHERE status = ?1 AND scheduled_at <= ?2 ORDER BY scheduled_at, id",
        )?;
        let rows = stmt.query_map(params![QUEUED, now], row_to_post)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn by_status(&self, status: &str) -> Result<Vec<(Post, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account, scheduled_at, images, caption, hook, status,
                    COALESCE(publish_id, ''), COALESCE(mode, '')
             FROM posts WHERE status = ?1 ORDER BY scheduled_at",
        )?;
        let rows = stmt.query_map(params![status], |r| {
            Ok((row_to_post(r)?, r.get::<_, String>(7)?, r.get::<_, String>(8)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn mark_sent(&self, id: &str, mode: Mode, now: i64, publish_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE posts SET status = ?1, mode = ?2, sent_at = ?3, publish_id = ?4 WHERE id = ?5",
            params![SENT, mode_str(mode), now, publish_id, id],
        )?;
        Ok(())
    }

    /// Claim a post before calling the platform, so a crash can't post twice.
    pub fn mark_sending(&self, id: &str, mode: Mode, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE posts SET status = ?1, mode = ?2, sent_at = ?3 WHERE id = ?4",
            params![SENDING, mode_str(mode), now, id],
        )?;
        Ok(())
    }

    /// Put a post back in the queue (it was not posted).
    pub fn requeue(&self, id: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE posts SET status = ?1, scheduled_at = ?2, sent_at = NULL, mode = NULL WHERE id = ?3",
            params![QUEUED, at, id],
        )?;
        Ok(())
    }

    pub fn set_status(&self, id: &str, status: &str, detail: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE posts SET status = ?1, detail = ?2 WHERE id = ?3",
            params![status, detail, id],
        )?;
        Ok(())
    }

    /// Derive the policy state for an account at `now`.
    pub fn account_state(&self, handle: &str, now: i64, utc_offset: i64) -> Result<AccountState> {
        let (paused, started_at): (bool, Option<i64>) = self
            .conn
            .query_row(
                "SELECT paused, started_at FROM accounts WHERE handle = ?1",
                params![handle],
                |r| Ok((r.get::<_, i64>(0)? != 0, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((false, None));
        let today = day_of(now, utc_offset);
        let day_start = today * crate::policy::SECS_PER_DAY - utc_offset;
        let posted_today: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE account = ?1 AND sent_at IS NOT NULL
             AND sent_at >= ?2 AND sent_at < ?3",
            params![handle, day_start, day_start + crate::policy::SECS_PER_DAY],
            |r| r.get(0),
        )?;
        let last_post_at: Option<i64> = self.conn.query_row(
            "SELECT MAX(sent_at) FROM posts WHERE account = ?1",
            params![handle],
            |r| r.get(0),
        )?;
        // TikTok counts pending inbox shares over a rolling 24 hours.
        let pending: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE account = ?1 AND mode = 'inbox'
             AND status = ?2 AND sent_at > ?3",
            params![handle, SENT, now - crate::policy::SECS_PER_DAY],
            |r| r.get(0),
        )?;
        Ok(AccountState {
            paused,
            started_at: started_at.unwrap_or(now),
            day: today,
            posted_today,
            last_post_at,
            pending,
        })
    }

    /// Set an account's first-post time if it has none (starts its ramp).
    pub fn ensure_started(&self, handle: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO accounts (handle, started_at) VALUES (?1, ?2)
             ON CONFLICT(handle) DO UPDATE SET started_at = COALESCE(started_at, ?2)",
            params![handle, now],
        )?;
        Ok(())
    }

    pub fn set_paused(&self, handle: &str, paused: bool, reason: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO accounts (handle, paused, reason) VALUES (?1, ?2, ?3)
             ON CONFLICT(handle) DO UPDATE SET paused = ?2, reason = ?3",
            params![handle, paused as i64, reason],
        )?;
        Ok(())
    }

    pub fn stopped(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT value FROM engine WHERE key = 'stopped'", [], |r| r.get::<_, String>(0))
            .optional()?
            .map(|v| v == "1")
            .unwrap_or(false))
    }

    pub fn set_stopped(&self, stopped: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO engine (key, value) VALUES ('stopped', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![if stopped { "1" } else { "0" }],
        )?;
        Ok(())
    }

    pub fn log(&self, now: i64, account: &str, post: &str, kind: &str, detail: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events (at, account, post, kind, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![now, account, post, kind, detail],
        )?;
        Ok(())
    }

    pub fn token(&self, handle: &str) -> Result<Option<(String, Option<String>, Option<i64>)>> {
        self.conn
            .query_row(
                "SELECT access_token, refresh_token, expires_at FROM tokens WHERE handle = ?1",
                params![handle],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn save_token(&self, handle: &str, access: &str, refresh: Option<&str>, expires_at: Option<i64>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tokens (handle, access_token, refresh_token, expires_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(handle) DO UPDATE SET access_token = ?2,
                refresh_token = COALESCE(?3, refresh_token), expires_at = ?4",
            params![handle, access, refresh, expires_at],
        )?;
        Ok(())
    }
}

fn row_to_post(r: &rusqlite::Row) -> rusqlite::Result<Post> {
    let images: String = r.get(3)?;
    Ok(Post {
        id: r.get(0)?,
        account: r.get(1)?,
        scheduled_at: r.get(2)?,
        images: serde_json::from_str(&images).unwrap_or_default(),
        caption: r.get(4)?,
        hook: r.get(5)?,
        status: r.get(6)?,
    })
}

pub fn mode_str(mode: Mode) -> &'static str {
    match mode {
        Mode::Direct => "direct",
        Mode::Inbox => "inbox",
    }
}
