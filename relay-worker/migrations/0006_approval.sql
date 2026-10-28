-- Direct posts need the creator's explicit approval and their chosen
-- settings (privacy, comments, commercial disclosure) before sending.
ALTER TABLE posts ADD COLUMN settings TEXT;
ALTER TABLE posts ADD COLUMN approved_at INTEGER;
