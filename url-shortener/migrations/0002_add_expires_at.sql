-- Optional link expiry. NULL = never expires. Stored as Unix seconds (UTC),
-- matching created_at. Lazily enforced on read and best-effort purged.
ALTER TABLE links ADD COLUMN expires_at INTEGER;
