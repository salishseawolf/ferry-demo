# The toolchain, with the same tool versions as mise.toml: shared by the
# release build below and by the dev server (the `dev` stage, compose.yaml).
FROM docker.io/library/debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f AS toolchain

# gcc + libc6-dev is all Rust build scripts need on the host side.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*

# Python is only for the dev servers; the `dev` stage adds it.
ENV MISE_VERSION=v2026.10.3 \
    MISE_YES=1 \
    MISE_DISABLE_TOOLS=python \
    PATH=/root/.local/bin:/root/.local/share/mise/shims:$PATH
RUN curl -fsSL https://mise.run | sh

WORKDIR /src
COPY mise.toml ./
RUN mise trust && mise install

# Dev server: the toolchain plus Python, with the source bind-mounted at
# /src and caches in volumes (see compose.yaml).
FROM toolchain AS dev
ENV MISE_DISABLE_TOOLS=
RUN mise install python
CMD ["trunk", "serve", "--address", "0.0.0.0", "--port", "8080"]

# The app: the wasm bundle, built for release
FROM toolchain AS build
COPY Cargo.toml Cargo.lock Trunk.toml index.html ./
COPY src ./src
COPY crates ./crates
COPY assets ./assets
# Cache mounts keep compiled deps and trunk's wasm-bindgen/wasm-opt downloads
# across builds without baking them into a layer.
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/root/.cache/trunk \
    --mount=type=cache,target=/src/target \
    mise run build \
 && gzip -k -9 dist/*.js dist/*.wasm

# The server (crates/server), built natively. Its migrations are compiled
# in. The app's sources come along only because they're in the workspace.
FROM toolchain AS server-build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY crates ./crates
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p ferry-server \
 && cp target/release/ferry-server /usr/local/bin/

# The server's image (`podman build --target server`), as non-root, on
# 8090. Configured from the environment: DATABASE_URL and WSDOT_API_KEY
# (Secrets in the cluster, `op run` locally); see crates/server/src/config.rs.
FROM docker.io/library/debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f AS server
# The system's certificates, for WSF's TLS.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=server-build /usr/local/bin/ferry-server /usr/local/bin/ferry-server
# Built beforehand with `mise run tiles:routes`.
COPY --chmod=644 tiles/routes/ferry-routes.json /srv/ferry-routes.json
ENV BIND=0.0.0.0:8090 \
    ROUTES_FILE=/srv/ferry-routes.json
USER 65532:65532
EXPOSE 8090
CMD ["ferry-server"]

# The app's image (the default target, or `--target web`): the static
# files, served as non-root on 8080, with /api/v1/ passed to the server.
FROM docker.io/nginxinc/nginx-unprivileged:1.29-alpine@sha256:0c79d56aee561a1d81c63f00eee5fb5fe29279560cdc55e91425133104c7fbe6 AS web
# Filled in at startup with the server's host:port (FERRY_API: by default
# the same pod, as a Kubernetes sidecar would be).
COPY --chmod=644 nginx.conf /etc/nginx/templates/default.conf.template
ENV NGINX_ENVSUBST_FILTER=^FERRY_ \
    FERRY_API=127.0.0.1:8090
COPY --from=build /src/dist /usr/share/nginx/html
# Built beforehand with `mise run tiles:elevation`.
COPY tiles /usr/share/nginx/html/tiles
EXPOSE 8080
