-- "Text me" requests from the landing page (behind a flag): visitors in an
-- in-app browser leave a phone number and we text them first.
CREATE TABLE IF NOT EXISTS textme (
  phone TEXT PRIMARY KEY,
  src TEXT,
  inapp TEXT,
  created_at INTEGER NOT NULL,
  ip_hash TEXT
);
CREATE INDEX IF NOT EXISTS textme_ip ON textme(ip_hash, created_at);
