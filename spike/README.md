# Research harnesses

These files reproduce design experiments. Production code must not import this
package. Results describe their particular corpus and prototype, not the
behavior or density of `metrics/`.

The durable engine's correctness tests now live beside it in `metrics/` and
`internal/sqlite/`. Historical prototype tests remain available until their
measurements have equivalent harnesses against the real engine. Moving or
deleting them prematurely would break the commands in the existing reports.

Synthetic measurements use `TINYSTORE_SPIKE=1`. Corpus probes use
`TINYSTORE_CORPUS` or `TINYSTORE_JSONL`; see the corresponding report in `docs/`.
Normal unit gates in this package still run without those variables.
