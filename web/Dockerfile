# The site as one image: the prerendered build, the Bun server that hands it
# out, and the tinystore binary that server's SDK starts as the store's
# sidecar. Built from the repository's top once the site is built:
#
#     (cd web && bun run build) && docker build -f web/Dockerfile .
#
# The site is built outside, where git can date each page; the image only
# packs it. The store lives in /data, a volume, so a new image keeps it.

FROM golang:1.27-alpine AS tinystore
WORKDIR /src
COPY go.mod go.sum ./
COPY server/go.mod server/go.sum ./server/
COPY cmd/tinystore/go.mod cmd/tinystore/go.sum ./cmd/tinystore/
RUN cd cmd/tinystore && go mod download
COPY . .
RUN cd cmd/tinystore && CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/tinystore .

# the server's dependencies alone: zod, and the SDK from this checkout, unpublished
FROM oven/bun:1.4.2-alpine AS dependencies
WORKDIR /src/web
COPY sdk/js /src/sdk/js
COPY web/package.json web/bun.lock ./
RUN bun install --frozen-lockfile --production

FROM oven/bun:1.4.2-alpine AS runtime
WORKDIR /src/web
ENV NODE_ENV=production
# the local default of 127.0.0.1 is a loopback nobody outside the container reaches
ENV HOST=0.0.0.0
ENV PORT=3000
ENV SITE_DIR=/src/web/build
ENV DATA_DIR=/data
ENV TINYSTORE_BIN=/usr/local/bin/tinystore
COPY --from=tinystore /out/tinystore /usr/local/bin/tinystore
# node_modules links into the SDK's source, so the source sits where the install saw it
COPY --from=dependencies /src/sdk/js /src/sdk/js
COPY --from=dependencies /src/web/node_modules ./node_modules
COPY web/package.json ./
COPY web/server ./server
COPY web/src/lib/events.ts ./src/lib/events.ts
COPY web/build ./build
RUN mkdir -p /data && chown bun:bun /data
USER bun
VOLUME /data
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:3000/api/ready || exit 1
# run it with --init: the store's sidecar is a child, and someone has to reap it
CMD ["bun", "server/index.ts"]
