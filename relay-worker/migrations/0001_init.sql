-- Same tables as the native Relay, plus config and OAuth state that the
-- native version kept in files or memory.
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
  config TEXT NOT NULL,
  paused INTEGER NOT NULL DEFAULT 0,
  started_at INTEGER,
  reason TEXT
);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tokens (
  handle TEXT PRIMARY KEY,
  access_token TEXT NOT NULL,
  refresh_token TEXT,
  expires_at INTEGER
);
CREATE TABLE IF NOT EXISTS oauth_states (state TEXT PRIMARY KEY, handle TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS events (at INTEGER NOT NULL, account TEXT, post TEXT, kind TEXT NOT NULL, detail TEXT);
