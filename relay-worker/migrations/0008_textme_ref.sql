-- A "Text me" request that came from a referral link keeps the code, so the
-- friend's Standard Grow chat is credited to the store that referred them.
-- person is Standard Grow's opaque id for the phone (the gateway's
-- grow-person/v1 hash), the only thing sent to Grow. ref_at is when the code
-- was added (a number asked first without one keeps its created_at), and
-- ref_sent_at is when Grow took it.
ALTER TABLE textme ADD COLUMN ref TEXT;
ALTER TABLE textme ADD COLUMN person TEXT;
ALTER TABLE textme ADD COLUMN ref_at INTEGER;
ALTER TABLE textme ADD COLUMN ref_sent_at INTEGER;
CREATE INDEX IF NOT EXISTS textme_ref_unsent ON textme(ref_sent_at, ref_at) WHERE ref IS NOT NULL;
