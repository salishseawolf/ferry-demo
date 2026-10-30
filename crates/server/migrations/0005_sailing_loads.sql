-- How full each sailing's car deck was as it left (estimated).
--
-- Space still open to reservations, on routes that take them (null where
-- WSF doesn't show it). WSF's drive-up and reservable counts overlap, so
-- the space left is their sum capped at the boat's capacity.
ALTER TABLE sailing_space ADD COLUMN reservable integer;
-- When WSF last gave this row's counts unchanged: a row is recorded when
-- something changes, and this says how long it stood.
ALTER TABLE sailing_space ADD COLUMN seen_until timestamptz;

-- Each scheduled trip with the count in effect as it left the dock: the
-- last for its departure (its destination's, failing that any), recorded
-- no later than a few minutes after it left (WSF drops a departure from
-- its counts about then). Cancelled sailings and those without counts
-- aren't loads; unscheduled trips have no departure to match.
-- `reading_age_min` is how long before it left that count was last seen:
-- WSF stops counting some terminals' sailings a few minutes early, so
-- cars may have boarded since.
CREATE VIEW sailing_loads AS
SELECT t.id AS trip_id, t.vessel_id, t.departing, t.arriving,
    t.scheduled_departure, t.left_dock,
    s.capacity, s.drive_up, s.reservable, s.oversold,
    LEAST(s.capacity, COALESCE(s.drive_up, 0) + COALESCE(s.reservable, 0)) AS free,
    1.0 - LEAST(s.capacity, COALESCE(s.drive_up, 0) + COALESCE(s.reservable, 0))::float8
        / s.capacity AS load,
    GREATEST(0.0, extract(epoch FROM t.left_dock - COALESCE(s.seen_until, s.observed_at))
        / 60.0)::float8 AS reading_age_min
FROM trips t
CROSS JOIN LATERAL (
    SELECT * FROM sailing_space s
    WHERE s.terminal_id = t.departing
      AND s.departs_at = t.scheduled_departure
      AND s.observed_at <= t.left_dock + interval '5 minutes'
      AND NOT s.cancelled AND s.capacity > 0 AND s.drive_up IS NOT NULL
    ORDER BY (s.destination_id = t.arriving) DESC, s.observed_at DESC
    LIMIT 1
) s;
