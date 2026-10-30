-- Every answer fetched from WSF, as received: the source of truth that
-- everything derived (live state, history) can be rebuilt from.
-- Append-only.

CREATE TABLE raw_polls (
    id bigserial PRIMARY KEY,
    -- The WSF path asked for, such as vessels/rest/vessellocations.
    path text NOT NULL,
    fetched_at timestamptz NOT NULL,
    -- The HTTP status, if a response came back.
    status smallint,
    -- The answer, when it was JSON.
    body jsonb,
    -- Why there's no usable answer, if there isn't.
    error text
);

-- Bodies are large and repetitive; lz4 compresses them quickly.
ALTER TABLE raw_polls ALTER COLUMN body SET COMPRESSION lz4;

CREATE INDEX raw_polls_path_time ON raw_polls (path, fetched_at);
