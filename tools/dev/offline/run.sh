#!/usr/bin/env bash
# The app with no network and no WSDOT key (`mise run dev:offline`): a
# throwaway Postgres on 127.0.0.1:5433, the fake WSF (fake_wsf.py) on 8095,
# the tiles on 8081, the server on 8090 and trunk on 8080, all run here
# rather than in containers (but for Postgres), so it works where compose
# doesn't. Stop the dev environment first: both want 8080.
#
#   DUMP=dumps/ferry-….dump  restore a dev:dump first, then rebuild its history
#   SEED_WEEKS=4             with DUMP: make up weeks of car counts from it
#                            (seed_history.sql), for advice and the charts
#
# Ctrl-C stops everything; the database goes with it. Logs are in
# target/offline/.
set -euo pipefail
cd "$(dirname "$0")/../../.."

here=tools/dev/offline
logs=target/offline
mkdir -p "$logs"
db=ferry-offline-db
# The same Postgres as the dev environment.
pg=$(grep -m1 -o 'docker.io/library/postgres:[^ ]*' compose.yaml)
export DATABASE_URL=postgres://ferry:ferry@127.0.0.1:5433/ferry
# The fake WSF takes any key; the real one is never looked up.
export WSDOT_API_KEY=FAKE
export WSF_BASE=http://127.0.0.1:8095/ferries/api/

stop() {
  trap - EXIT
  kill $(jobs -p) 2>/dev/null || true
  podman stop -t 2 "$db" >/dev/null 2>&1 || true
}
trap stop EXIT
trap 'exit 130' INT TERM

pg() { podman run --rm -i --network host -e PGPASSWORD=ferry "$pg" "$@"; }

echo "Starting Postgres on 127.0.0.1:5433"
podman run -d --rm --name "$db" --network host \
  -e POSTGRES_USER=ferry -e POSTGRES_PASSWORD=ferry -e POSTGRES_DB=ferry \
  "$pg" -c port=5433 -c listen_addresses=127.0.0.1 >/dev/null
until pg pg_isready -q -h 127.0.0.1 -p 5433; do sleep 1; done

cargo build --locked -q -p ferry-server
server=target/debug/ferry-server

if [[ -n ${DUMP:-} ]]; then
  echo "Restoring $DUMP"
  pg pg_restore -h 127.0.0.1 -p 5433 -U ferry -d ferry --no-owner < "$DUMP"
  # The derivations may have changed since the dump; this also migrates.
  "$server" rebuild > "$logs/rebuild.log" 2>&1
  if [[ -n ${SEED_WEEKS:-} ]]; then
    echo "Making up $SEED_WEEKS weeks of history"
    pg psql -h 127.0.0.1 -p 5433 -U ferry -d ferry -qX -v ON_ERROR_STOP=1 \
      -v weeks="$SEED_WEEKS" < "$here/seed_history.sql"
  fi
  "$server" compute-advice > "$logs/advice.log" 2>&1
fi

python3 -I "$here/fake_wsf.py" 8095 > "$logs/fake_wsf.log" 2>&1 &
python3 -I tools/dev/tiles_server.py 8081 tiles > "$logs/tiles.log" 2>&1 &
# The server's first polls would fail (and wait a minute to try again).
until curl -sf -o /dev/null "${WSF_BASE}schedule/rest/cacheflushdate"; do sleep 0.2; done
RUST_LOG=info "$server" > "$logs/server.log" 2>&1 &
trunk serve --address 127.0.0.1 --port 8080 > "$logs/trunk.log" 2>&1 &

echo "Building the app and loading schedules (trunk: $logs/trunk.log)"
# Schedules wait a minute on a fresh database, for the route network.
until grep -q success "$logs/trunk.log" && curl -sf -o /dev/null http://127.0.0.1:8080/ \
  && curl -sf -o /dev/null http://127.0.0.1:8090/readyz \
  && curl -sf -o /dev/null "http://127.0.0.1:8090/api/v1/schedule?route=fauntleroy-vashon-southworth"; do
  # A job that died (a port in use, say) won't come back.
  if [[ $(jobs -rp | wc -l) -lt 4 ]]; then
    echo "Something stopped: see $logs/" >&2
    exit 1
  fi
  sleep 2
done
echo "Ready on http://127.0.0.1:8080 (Ctrl-C stops it)"
wait
