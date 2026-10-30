-- Arrival advice, worked out ahead (advice.rs).
--
-- Each terminal's record of past sailings (counts, boat, how far counted)
-- for the sailings scheduled before `through_ms`, written by
-- `ferry-server compute-advice` (a daily job). The server loads the
-- latest, adds the counts since, and publishes advice every minute.
CREATE TABLE advice_history (
    terminal_id integer PRIMARY KEY,
    computed_at timestamptz NOT NULL,
    through_ms bigint NOT NULL,
    records jsonb NOT NULL
);

-- The advice the server published for each sailing, recorded when it
-- changed: what riders saw, and when. The advice check scores it.
CREATE TABLE advice_given (
    terminal_id integer NOT NULL,
    departs_at timestamptz NOT NULL,
    published_at timestamptz NOT NULL,
    advice jsonb NOT NULL,
    PRIMARY KEY (terminal_id, departs_at, published_at)
);
