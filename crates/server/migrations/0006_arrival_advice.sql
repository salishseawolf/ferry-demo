-- For advice on when to arrive (advice.rs).
--
-- How many cars were waiting beyond the space (WSF's count below zero),
-- where it was oversold; drive_up is then 0.
ALTER TABLE sailing_space ADD COLUMN oversold_by integer;

-- Days whose traffic isn't typical besides the holidays the server knows
-- (calendar.rs), such as a big game or a festival, kept out of the usual
-- statistics and given extra time in advice. Added by hand:
--   INSERT INTO special_days VALUES ('2026-12-05', 'Apple Cup');
CREATE TABLE special_days (
    day date PRIMARY KEY,
    name text NOT NULL
);
