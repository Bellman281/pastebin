-- Links table. `code` is the primary key, giving us a unique index for free
-- and turning a duplicate insert into a constraint violation we map to Conflict.
CREATE TABLE IF NOT EXISTS links (
    code       TEXT    NOT NULL PRIMARY KEY,
    target     TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    hits       INTEGER NOT NULL DEFAULT 0
);
