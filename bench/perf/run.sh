#!/bin/sh
# run one phase of the performance profile. Phase A fills the databases the read
# phases share and is deliberately parallel; its own timings are not a result.
# Every later phase runs one stage at a time, because several timed stages at
# once measure the scheduler instead of the engine.
set -e
P=/perf/perf

case "$1" in
populate)
	rm -rf /perf/s1k /perf/s10k /perf/s100k
	$P -dir /perf/s1k -stage populate -series 1000 -samples 2000 &
	$P -dir /perf/s10k -stage populate -series 10000 -samples 500 &
	$P -dir /perf/s100k -stage populate -series 100000 -samples 50 &
	wait
	;;
ingest)
	rm -rf /perf/w
	$P -dir /perf/w -stage ingest -label batch_1 -series 1000 -samples 5 -batch 1
	$P -dir /perf/w -stage ingest -label batch_100 -series 1000 -samples 200 -batch 100
	$P -dir /perf/w -stage ingest -label batch_10000 -series 1000 -samples 2000 -batch 10000
	$P -dir /perf/w -stage ingest -label series_1k -series 1000 -samples 2000 -batch 10000
	$P -dir /perf/w -stage ingest -label series_10k -series 10000 -samples 200 -batch 10000
	$P -dir /perf/w -stage ingest -label series_100k -series 100000 -samples 20 -batch 10000
	$P -dir /perf/w -stage ingest -label no_maintenance -series 1000 -samples 2000 -batch 10000 -maintain-every 0
	;;
read_shapes)
	for shape in point hour day series_full selector_low_cardinality selector_high_cardinality scan_all; do
		$P -dir /perf/s1k -stage read -label s1k -series 1000 -shape "$shape" -readers 1 -seconds 5
	done
	;;
read_scale)
	for shape in point hour selector_low_cardinality selector_high_cardinality; do
		$P -dir /perf/s10k -stage read -label s10k -series 10000 -shape "$shape" -readers 1 -seconds 5
		$P -dir /perf/s100k -stage read -label s100k -series 100000 -shape "$shape" -readers 1 -seconds 5
	done
	;;
read_readers)
	for r in 1 2 4 8; do
		$P -dir /perf/s10k -stage read -label s10k -series 10000 -shape hour -readers "$r" -seconds 5
	done
	for r in 1 2 4 8; do
		$P -dir /perf/s10k -stage read -label s10k -series 10000 -shape selector_low_cardinality -readers "$r" -seconds 5
	done
	;;
mixed)
	for r in 1 2 4 8; do
		$P -dir /perf/s1k -stage mixed -label s1k -series 1000 -readers "$r" -seconds 15
	done
	;;
*)
	echo "usage: run.sh populate|ingest|read_shapes|read_scale|read_readers|mixed" >&2
	exit 2
	;;
esac
