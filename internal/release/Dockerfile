# The server image a release publishes, ghcr.io/tinyshed/tinystore: the
# release's own linux binary, taken out of its archive, on distroless static,
# which has /tmp for SQLite's temporary files and a user without privileges.
# The release workflow lays out its context: linux-<arch>/tinystore and an
# empty data/.
FROM gcr.io/distroless/static-debian12:nonroot
ARG TARGETARCH
COPY linux-${TARGETARCH}/tinystore /usr/local/bin/tinystore
# the store's directory, the server's user its owner, as a volume mounted on
# it starts
COPY --chown=nonroot:nonroot data /data
VOLUME /data
EXPOSE 7443
ENTRYPOINT ["/usr/local/bin/tinystore"]
CMD ["serve", "--dir", "/data", "--listen", "tls://0.0.0.0:7443", \
	"--tls-cert", "/etc/tinystore/cert.pem", "--tls-key", "/etc/tinystore/key.pem", \
	"--tokens", "/etc/tinystore/tokens"]
