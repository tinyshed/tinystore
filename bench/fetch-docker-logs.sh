#!/bin/sh
# fetch-docker-logs.sh <corpus> <ssh target>...
#
# Copies every container's json-file log from each host, read-only: one tar stream
# per host over ssh, nothing written on the host. Hosts become host-a, host-b, …
# so no address or hostname enters the corpus layout.
#
#   <corpus>/host-a/containers.txt   /name|image|log path, from docker inspect
#   <corpus>/host-a/raw/<id>/<id>-json.log, .1, .2 …
#
# The logs are production data: keep the corpus local, never commit or publish it,
# and report only aggregates. TestRecordV2DockerLogs reads it through
# TINYSTORE_RECORD_DOCKER=<corpus>.
set -eu

if [ "$#" -lt 2 ]; then
	echo "usage: $0 <corpus> <ssh target>..." >&2
	exit 2
fi
corpus=$1
shift
index=0
for target in "$@"; do
	index=$((index + 1))
	host="host-$(printf "\\$(printf %o $((96 + index)))")"
	mkdir -p "$corpus/$host/raw"
	ssh -C -o BatchMode=yes "$target" \
		"docker inspect --format '{{.Name}}|{{.Config.Image}}|{{.LogPath}}' \$(docker ps -aq)" \
		>"$corpus/$host/containers.txt"
	ssh -C -o BatchMode=yes "$target" \
		"cd /var/lib/docker/containers && tar -cf - */*-json.log* 2>/dev/null" |
		tar -C "$corpus/$host/raw" -xf -
	echo "$host: $(find "$corpus/$host/raw" -type f | wc -l) files"
done
( cd "$corpus" && find . -path '*/raw/*' -type f -exec sha256sum {} + | sort -k2 | sha256sum )
