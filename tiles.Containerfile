# The terrain tiles, as an image of their own: built and pushed from the
# machine that has the source rasters (`mise run tiles:publish`), and
# copied into the app's image from there (Containerfile, `ARG TILES`).
# Built with tiles/ as the context.
FROM scratch
LABEL org.opencontainers.image.source="https://github.com/salishseawolf/ferry-demo" \
      org.opencontainers.image.description="ferry-demo's terrain tiles (sources and their terms are credited in the app)"
COPY . /tiles/
