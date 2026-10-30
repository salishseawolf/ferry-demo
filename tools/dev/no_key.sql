-- Fails (psql exits non-zero, with ON_ERROR_STOP) if any recorded WSF
-- answer or error holds the WSDOT key, read from the environment so it's
-- never on a command line. An empty or missing key fails too. Run by
-- `mise run dev:dump` before it writes a dump.
\getenv key WSDOT_API_KEY
SELECT EXISTS (
    SELECT 1 FROM raw_polls
    WHERE strpos(coalesce(body::text, '') || coalesce(error, ''), :'key') > 0
) AS found \gset
-- Not a CASE in the query: the planner folds a constant 1/0 before
-- looking at the condition.
\if :found
SELECT 1/0;
\endif
