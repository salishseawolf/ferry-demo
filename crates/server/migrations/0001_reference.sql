-- Reference data: the terminals and boats (from WSF), the route network
-- (OpenStreetMap legs, grouped into the routes riders know), and what's
-- been learned about each slip. Changes rarely.

CREATE TABLE terminals (
    id integer PRIMARY KEY,
    name text NOT NULL,
    lon double precision NOT NULL,
    lat double precision NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE vessels (
    id integer PRIMARY KEY,
    name text NOT NULL,
    class text,
    length_m double precision,
    beam_m double precision,
    car_capacity integer,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- A leg as OpenStreetMap draws it (id: its way id), between the terminals
-- its ends reach, if both do; a slip approach starts and ends at one.
CREATE TABLE legs (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    terminal_a integer REFERENCES terminals,
    terminal_b integer REFERENCES terminals,
    length_m double precision NOT NULL,
    -- Cubic Béziers end to end, each four [lon, lat] control points.
    curves jsonb NOT NULL
);

-- The routes riders know, such as fauntleroy-vashon-southworth.
CREATE TABLE routes (
    id text PRIMARY KEY,
    name text NOT NULL
);

CREATE TABLE route_legs (
    route_id text NOT NULL REFERENCES routes ON DELETE CASCADE,
    leg_id bigint NOT NULL REFERENCES legs ON DELETE CASCADE,
    PRIMARY KEY (route_id, leg_id)
);

CREATE TABLE route_terminals (
    route_id text NOT NULL REFERENCES routes ON DELETE CASCADE,
    terminal_id integer NOT NULL REFERENCES terminals,
    PRIMARY KEY (route_id, terminal_id)
);

-- Which way each slip opens, learned from boats seen leaving it.
CREATE TABLE terminal_facing (
    terminal_id integer PRIMARY KEY REFERENCES terminals,
    heading_deg double precision NOT NULL,
    samples integer NOT NULL,
    observed_at timestamptz NOT NULL
);

-- When each WSF service's slowly changing data last changed, as its
-- cacheflushdate said at our last fetch (epoch ms), so a refresh can skip
-- fetching what hasn't changed.
CREATE TABLE source_state (
    source text PRIMARY KEY,
    flushed_ms bigint NOT NULL,
    fetched_at timestamptz NOT NULL DEFAULT now()
);
