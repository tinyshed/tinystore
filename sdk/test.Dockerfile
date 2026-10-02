# The Linux both SDKs' suites run in from any host: task sdk:linux. Go builds
# the tinystore they test against; bun, node and uv come from their makers'
# images. The JS SDK's suite runs under Node too, and node runs pyright.
FROM golang:1.27
COPY --from=oven/bun:1.4.2 /usr/local/bin/bun /usr/local/bin/bun
COPY --from=node:24-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=ghcr.io/astral-sh/uv:0.11.26 /uv /uvx /usr/local/bin/
