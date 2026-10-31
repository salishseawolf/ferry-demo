-- Makes up :weeks weeks of car-deck history from a restored dump, so
-- arrival advice and the load charts have something to work from offline:
-- copies of the counts around the dump's last recording, moved by whole
-- days onto the same day of the week as today, one to :weeks weeks back.
-- Run by run.sh (SEED_WEEKS=n) after it restores DUMP and rebuilds.
INSERT INTO sailing_space (terminal_id, departs_at, destination_id, observed_at,
    seen_until, vessel_id, drive_up, capacity, oversold, oversold_by, cancelled, reservable)
SELECT terminal_id, departs_at + shift, destination_id, observed_at + shift,
    seen_until + shift, vessel_id, drive_up, capacity, oversold, oversold_by, cancelled, reservable
FROM sailing_space,
    (SELECT max(observed_at) AS last FROM sailing_space) AS dump,
    LATERAL (
        SELECT make_interval(days => (now() AT TIME ZONE 'America/Los_Angeles')::date
            - (dump.last AT TIME ZONE 'America/Los_Angeles')::date - 7 * k) AS shift
        FROM generate_series(1, :weeks) AS k
    ) AS weeks
WHERE departs_at BETWEEN dump.last - interval '1 day' AND dump.last + interval '1 day'
ON CONFLICT DO NOTHING;
