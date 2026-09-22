#!/bin/sh
set -eu
repo=$1
scratch=$2
fixture=$3
test ! -e "$scratch"
mkdir -p "$scratch/source"
git -C "$repo" -c safe.directory="$repo" archive f2974bc | tar -x -C "$scratch/source"
cd "$scratch/source/bench/perf"
sed -i '/w := &watcher{stop: make(chan struct{}), done: make(chan struct{})}/a\ if os.Getenv("TINYSTORE_PERF_NO_WATCH") == "1" { close(w.done); return w }' main.go
go build -o "$scratch/perf" .
for readers in 1 8; do
    for disabled in 0 1; do
        TINYSTORE_PERF_NO_WATCH=$disabled "$scratch/perf" -dir "$fixture" \
            -stage read -label "no_watch_$disabled" -series 10000 -shape hour \
            -readers "$readers" -seconds 3
    done
done
TINYSTORE_PERF_NO_WATCH=1 "$scratch/perf" -dir "$fixture" -stage read \
    -series 10000 -shape hour -readers 8 -seconds 5 \
    -cpu-profile "$scratch/read.cpu" -mutex-profile "$scratch/read.mutex"
go tool pprof -top -cum "$scratch/perf" "$scratch/read.cpu"
go tool pprof -top -cum "$scratch/perf" "$scratch/read.mutex"
