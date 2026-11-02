#!/usr/bin/env bash
# Runs the release images the way the Helm chart's pods run them
# (charts/ferry-demo): as their users, with a read-only root filesystem, no
# capabilities, and only the chart's emptyDirs writable. Then checks the
# server is ready and the app serves its page and passes /api/v1/ to the
# server. Catches what the chart's lint can't see: a file in an image its
# user can't read, or a place it must write.
#
#   DATABASE_URL=postgres://… mise run container:check
#
# IMAGE and SERVER_IMAGE name the images (mise.toml's defaults are the local
# builds); CONTAINER is the engine, podman unless set. Both containers use
# the host's network, on 8090 and 8080.
set -euo pipefail

engine=${CONTAINER:-podman}
server=ferry-check-server
web=ferry-check-web

stop() { "$engine" rm -f "$server" "$web" >/dev/null 2>&1 || true; }
logs() {
  for c in "$server" "$web"; do
    echo "--- $c" >&2
    "$engine" logs "$c" 2>&1 | tail -40 >&2
  done
}
trap stop EXIT
stop

# As the cluster's restricted pods. Podman would otherwise mount its own
# tmpfs on /tmp, /var/tmp and /run under --read-only, which a pod doesn't
# have. The tmpfs modes are emptyDir's.
pod=(--network host --read-only --cap-drop ALL --security-opt no-new-privileges)
[[ $engine == podman ]] && pod+=(--read-only-tmpfs=false)

"$engine" run -d --name "$server" "${pod[@]}" --user 65532:65532 \
  -e DATABASE_URL -e BIND=127.0.0.1:8090 "$SERVER_IMAGE" >/dev/null
"$engine" run -d --name "$web" "${pod[@]}" --user 101:101 \
  --tmpfs /tmp:mode=1777 --tmpfs /etc/nginx/conf.d:mode=1777 \
  -e FERRY_API=127.0.0.1:8090 "$IMAGE" >/dev/null

answers() {
  for _ in $(seq 60); do
    curl -sf -o /dev/null "$1" && return 0
    sleep 1
  done
  echo "check_images: no answer from $1" >&2
  logs
  exit 1
}
answers http://127.0.0.1:8090/readyz
answers http://127.0.0.1:8080/healthz
answers http://127.0.0.1:8080/
answers http://127.0.0.1:8080/api/v1/network

if { "$engine" logs "$server"; "$engine" logs "$web"; } 2>&1 | grep -q 'Permission denied'; then
  echo "check_images: something was denied:" >&2
  logs
  exit 1
fi
echo "check_images: both images run as the chart runs them"
