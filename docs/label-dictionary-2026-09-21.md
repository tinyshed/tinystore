# Label dictionary in the real engine

Migration 0005 replaces repeated `(name,value,series_id)` postings with
`(label_id,series_id)`. The dictionary and its unique `(name,value)` index are
included in every size below. Series retain their original canonical labels;
queries resolve an exact dictionary entry before reading postings. Registration
and migration are transactional.

Same regenerated TSBS corpus and environment as
[the packed-head run](packed-head-2026-09-21.md): Go 1.27.1 linux/amd64,
2020 series, 5090400 samples, unchanged 242400 mutable head samples. Public
Ingest/Maintain/Close/Open/Read; every timestamp and float bit checked.

| Complete engine file | Bytes | B/sample |
|---|---:|---:|
| Before dictionary | 5488640 | 1.078234 |
| With dictionary | **5099520** | **1.001792** |

Another **7.1%** off the real file. Postings shrink from 663552 to 249856 bytes;
the dictionary and its index add 12288 bytes each. Other data remain present.
Full-file size is measured after Close, including any free pages.

The prior VM full directory was 5484522 bytes: this file is about 7.0% smaller
than that historical number. VM was not rerun, its TSBS float round trip was
not bit-exact, and this does not compare uncached data or live throughput.
The best research TinyStore layout was 4919296 bytes; the real engine is now
3.7% larger while preserving its durable mutable head and postings.

Normal tests, lint and Linux race tests passed. The schema-one upgrade test
also exercises dictionary migration. A new test verifies label reuse and
multi-label matching. No dependency or API change. Dictionary entries follow
the registry lifetime; public series deletion and dictionary GC remain future work.

Reproduce with `bench/run-tsbs.ps1 -Corpus ./bench/corpus/tsbs-packed -SkipEngines`.
No new Alibaba or competitor measurement was performed. No commit was made.
