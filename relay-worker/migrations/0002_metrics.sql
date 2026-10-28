-- Results per post, read back from TikTok with the video.list scope.
ALTER TABLE posts ADD COLUMN post_id TEXT;
ALTER TABLE posts ADD COLUMN views INTEGER;
ALTER TABLE posts ADD COLUMN likes INTEGER;
ALTER TABLE posts ADD COLUMN comments INTEGER;
ALTER TABLE posts ADD COLUMN shares INTEGER;
ALTER TABLE posts ADD COLUMN metrics_at INTEGER;
