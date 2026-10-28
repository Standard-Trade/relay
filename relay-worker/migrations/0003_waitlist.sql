CREATE TABLE IF NOT EXISTS waitlist (
  email TEXT PRIMARY KEY,
  site TEXT,
  src TEXT,
  created_at INTEGER NOT NULL,
  ip_hash TEXT
);
CREATE INDEX IF NOT EXISTS waitlist_ip ON waitlist(ip_hash, created_at);
