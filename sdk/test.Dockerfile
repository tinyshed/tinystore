# The Linux both SDKs' suites run in from any host: task sdk:linux. Go builds
# the tinystore they test against; bun and uv come from their makers' images,
# and Debian's node runs pyright.
FROM golang:1.27
COPY --from=oven/bun:1.4.2 /usr/local/bin/bun /usr/local/bin/bun
COPY --from=ghcr.io/astral-sh/uv:0.11.26 /uv /uvx /usr/local/bin/
RUN apt-get update \
	&& apt-get install -y --no-install-recommends nodejs \
	&& rm -rf /var/lib/apt/lists/*
