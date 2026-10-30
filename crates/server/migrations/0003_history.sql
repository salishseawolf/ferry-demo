-- History, derived from raw_polls (by each poll as it arrives, or all at
-- once by `ferry-server rebuild`), so it can always be rebuilt.

-- Each boat report, once.
CREATE TABLE vessel_positions (
    vessel_id integer NOT NULL,
    reported_at timestamptz NOT NULL,
    lon double precision NOT NULL,
    lat double precision NOT NULL,
    speed_kn real NOT NULL,
    heading_deg real NOT NULL,
    at_dock boolean NOT NULL,
    in_service boolean NOT NULL,
    departing integer,
    arriving integer,
    PRIMARY KEY (vessel_id, reported_at)
);
CREATE INDEX vessel_positions_time ON vessel_positions (reported_at);

-- A crossing: opened when a boat is seen leaving, closed when it's first
-- seen docked at the terminal it was bound for.
CREATE TABLE trips (
    id bigserial PRIMARY KEY,
    vessel_id integer NOT NULL,
    departing integer NOT NULL,
    arriving integer NOT NULL,
    scheduled_departure timestamptz,
    left_dock timestamptz NOT NULL,
    -- WSF's latest estimate while under way.
    eta timestamptz,
    arrived_at timestamptz,
    UNIQUE (vessel_id, left_dock)
);
CREATE INDEX trips_pair ON trips (departing, arriving, left_dock);

-- Drive-up space on a departure for one destination, recorded when it
-- changes (destination 0: WSF named none, as for some cancellations).
CREATE TABLE sailing_space (
    terminal_id integer NOT NULL,
    departs_at timestamptz NOT NULL,
    destination_id integer NOT NULL,
    observed_at timestamptz NOT NULL,
    vessel_id integer,
    drive_up integer,
    capacity integer,
    oversold boolean NOT NULL,
    cancelled boolean NOT NULL,
    PRIMARY KEY (terminal_id, departs_at, destination_id, observed_at)
);

-- Notices, with when they were first and last posted.
CREATE TABLE notices (
    id text PRIMARY KEY,
    title text NOT NULL,
    text text NOT NULL,
    updated_at timestamptz,
    first_seen timestamptz NOT NULL,
    last_seen timestamptz NOT NULL
);

CREATE TABLE notice_terminals (
    notice_id text NOT NULL REFERENCES notices ON DELETE CASCADE,
    terminal_id integer NOT NULL,
    PRIMARY KEY (notice_id, terminal_id)
);
