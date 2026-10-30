-- Trips on which WSF names no destination or scheduled departure, as for
-- a route's unscheduled bonus boat (Salish on Fauntleroy / Vashon /
-- Southworth in fall 2026): the destination is filled in when the boat
-- docks. Unscheduled trips count toward crossing times but have nothing to
-- be late against, so lateness leaves them out.
ALTER TABLE trips ALTER COLUMN arriving DROP NOT NULL;
ALTER TABLE trips
    ADD COLUMN unscheduled boolean GENERATED ALWAYS AS (scheduled_departure IS NULL) STORED;
