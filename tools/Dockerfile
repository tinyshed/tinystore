# The server image a release publishes, ghcr.io/tinyshed/tinystore: the glibc
# executable of the release's archive for each architecture, on distroless's
# image with glibc and nothing else. The release workflow puts the executables
# in amd64/ and arm64/ beside this file, and an empty data/.
#
#     docker run -v data:/data -v ./tokens:/etc/tinystore/tokens -p 7443:7443 ghcr.io/tinyshed/tinystore \
#         serve /data --listen tcp://0.0.0.0:7443 --tokens /etc/tinystore/tokens
FROM gcr.io/distroless/cc-debian12:nonroot
ARG TARGETARCH
COPY ${TARGETARCH}/tinystore /usr/local/bin/tinystore
# a volume takes its first owner from the directory it mounts over: nonroot's,
# so that the store can write there
COPY --chown=65532:65532 data /data
VOLUME /data
ENTRYPOINT ["/usr/local/bin/tinystore"]
CMD ["serve", "/data"]
