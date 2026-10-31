-- The published schedule through time, derived from raw_polls' schedule
-- answers: one row for each version of a sailing WSF has listed, seen from
-- first_seen to last_seen. A sailing retimed, reassigned to another boat or
-- given other footnotes is one row withdrawn and another added; a sailing
-- WSF drops, or a route it stops listing for the day, is withdrawn with
-- nothing after it (and if it comes back, there's a new row). So a trip
-- day's original schedule is its rows by earliest first_seen, and what's
-- published now is those with no withdrawn_at.
CREATE TABLE published_sailings (
    trip_date date NOT NULL,
    -- WSF's route id, not ours: its schedules are per its own routes.
    wsf_route integer NOT NULL,
    departing integer NOT NULL,
    arriving integer NOT NULL,
    departs_at timestamptz NOT NULL,
    arrives_at timestamptz,
    vessel_id integer,
    vessel text NOT NULL,
    loading text NOT NULL,
    -- Footnotes, as text: WSF's numbering isn't stable between answers.
    notes jsonb NOT NULL,
    first_seen timestamptz NOT NULL,
    last_seen timestamptz NOT NULL,
    withdrawn_at timestamptz,
    PRIMARY KEY (trip_date, wsf_route, departing, arriving, departs_at, vessel, first_seen)
);

CREATE INDEX published_sailings_open ON published_sailings (trip_date, wsf_route)
    WHERE withdrawn_at IS NULL;
