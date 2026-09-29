# Statements without their contexts' goroutines — 2026-09-29

[The measurement of the built server](rpc-server-2026-09-29.md) found that a
point read whose context can end costs a goroutine in `database/sql`, and
proposed running it without the context's cancel. Building it found a second
goroutine, the driver's (`interruptOnDone` in modernc, for every query and
exec), and the same pair on every statement of a grouped write, whose context
is the group's hold or its caller's. This round measures the server with both
gone against the server before them, on the same machine, in one session, and
the prototype's best sidecar beside it.

| Question | Answer | What follows |
|---|---|---|
| a point read with a call's own context, embedded | at one in flight 120,165 to 131,293 gets a second against 62,301 to 62,957 before, as fast as `context.Background` in the same pass; at 64 and 256 in flight 264,587 to 369,923 against 190,354 to 252,878, 27 to 49 % more | [the goroutines](#a-point-read-embedded) were the cost; what is left at depth is a hypothesis |
| gets through the server at 64 and 256 in flight | from Go 6 to 18 % more than before, from Bun 0 to 15 %; Python asyncio −2 to 12 %, its own loop its ceiling | [Gets](#gets) |
| sets through the server at 256 in flight | 33,541 to 35,529 a second against 26,241 to 28,346 before, 18 to 35 % more; at one in flight the same | [Sets](#sets): the writer's CPU a statement counted where the group is large |
| against the prototype's best sidecar | 364,930 and 365,792 gets a second against 421,049 and 435,930, 84 to 87 %, where the report before measured 72 to 77 %; CPU a get 24.0 to 25.9 µs against 18.1, where it was 33.7 to 34.5 in this session before | [the server's own allocations](#against-the-prototypes-best-sidecar) are what remains, 59 a get against 52 |

## Environment and reproduction

- **Base** `2003e1c`, the server as [the report before](rpc-server-2026-09-29.md)
  measured it. **Candidate** `fccf0ed`: `b99bc73` (a handler settled by what
  it returned), `1219c7f` (`sqlite.QueryRowByKey`), `4d5dcba` (a grouped
  write's statements without their caller's context, and a savepoint's
  without the hold's), `2f061b9`, `e93a731`, `7c3f3b1`, `ffe927f`, `4257e5c`
  and their documents and comments. The harness is `server/spike` and
  `spike/rpc_*` as both commits hold them, unchanged since `1885baf`.
- **Windows.** AMD Ryzen 7 7700, 8 cores / 16 logical processors, 31.1 GiB,
  one Samsung SSD 990 PRO 1 TB (NVMe), Windows 11 Pro 10.0.26200 with NTFS and
  Defender's real-time protection on; go1.27.1 windows/amd64, Bun 1.4.2,
  CPython 3.13.15; the stores in the temporary directory on that NVMe. Not an
  idle machine: an IDE and other sessions were open, at about 17 % of the
  processors before the first run, and no build or test ran beside the
  measurement.
- **Two seconds a case, everything twice**, the base first in each pair; a
  latency is the calls in flight over the rate, a mean, as the round gives it.
- **The order of the runs, 29 September, UTC**: from 10:13:10 to 10:19:32
  `TestGetsWithAContextThatCanEnd`, `TestServeTransports` and
  `TestServeProfile`, each pass the base and then the candidate; from 10:20
  to 10:22 the prototype's `TestRPCServerCost` and the candidate's
  `TestServeProfile` alternating, twice. The output is
  [data/rpc-contexts-2026-09-29-windows.txt](data/rpc-contexts-2026-09-29-windows.txt),
  the user's temporary directory replaced by `<tmp>`.

```sh
# the base and the candidate as worktrees of <repo>, each test binary built once
git -C <repo> worktree add --detach <base> 2003e1c
git -C <repo> worktree add --detach <candidate> fccf0ed
GOWORK=off go -C <tree>/server test -c -o <tmp>/<tree>.test.exe ./spike

# each test, each pass, the base's and then the candidate's, from <tree>/server/spike
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 <tmp>/<tree>.test.exe -test.run '^TestGetsWithAContextThatCanEnd$' -test.v -test.count=1
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 <tmp>/<tree>.test.exe -test.run '^TestServeTransports$' -test.v -test.count=1 -test.timeout 60m
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 <tmp>/<tree>.test.exe -test.run '^TestServeProfile$' -test.v -test.count=1

# the prototype, built with the candidate's engines, from <candidate>/spike, beside the candidate's profile
GOWORK=off go -C <candidate> test -c -o <tmp>/proto.test.exe ./spike
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 <tmp>/proto.test.exe -test.run '^TestRPCServerCost$' -test.v -test.count=1
```

## A point read, embedded

`TestGetsWithAContextThatCanEnd`: the round's `Get`s on the embedded bucket,
once with `context.Background` and once with each call's own
`context.WithCancel` of a longer-lived one, as the server derives a stream's
from its connection's. `Get`s a second, the mean latency at one in flight in
brackets:

| | | 1 | 64 | 256 |
|---|---|---:|---:|---:|
| base | `context.Background` | 128,260; 127,414 (7.8 µs) | 354,179; 353,505 | 312,872; 312,286 |
| | a call's own, which can end | 62,301; 62,957 (15.9–16.1 µs) | 252,878; 250,984 | 190,354; 204,168 |
| candidate | `context.Background` | 119,982; 127,269 (7.9–8.3 µs) | 411,883; 382,870 | 347,644; 298,568 |
| | a call's own, which can end | 131,293; 120,165 (7.6–8.3 µs) | 369,923; 320,264 | 283,609; 264,587 |

- **At one in flight the context costs nothing now**: a get takes 7.6 to 8.3
  µs with a call's own context and 7.9 to 8.3 with `context.Background`, where
  the base took 15.9 to 16.1 against 7.8.
- **At depth it costs 10 to 18 % of the same pass's `context.Background`**,
  where it cost 28 to 39 %. What remains is a hypothesis: each call's
  `context.WithCancel` registers with, and leaves, one parent that every
  caller shares, a lock the store does not take; no measurement here
  separates it from the others.
- `context.Background` was 8 to 16 % faster at 64 in the candidate, and its
  two passes at 256 differ by 16 %; its path changed only by a check of the
  context before the statement, which cannot make it faster, so the builds'
  difference there is taken as the machine's.

## Gets

`Get`s a second through one connection, the directory's own transport, a
named pipe, stdio and TCP, the mean latency at one in flight in brackets:

| client | transport | | 1 in flight | 64 | 256 |
|---|---|---|---:|---:|---:|
| Go | named pipe | base | 22,314; 22,480 (44.5–44.8 µs) | 292,888; 295,560 | 305,782; 303,983 |
| | | candidate | 23,980; 25,868 (38.7–41.7 µs) | 334,898; 338,673 | 341,248; 349,866 |
| Go | stdio | base | 20,967; 24,292 (41.2–47.7 µs) | 302,711; 317,286 | 325,378; 317,200 |
| | | candidate | 22,108; 24,657 (40.6–45.2 µs) | 336,656; 354,573 | 361,871; 352,681 |
| Go | TCP | base | 13,130; 14,029 (71.3–76.2 µs) | 295,759; 296,220 | 313,986; 312,351 |
| | | candidate | 15,304; 13,635 (65.3–73.3 µs) | 340,160; 339,426 | 367,834; 366,943 |
| Bun | named pipe | base | 21,989; 19,993 (45.5–50.0 µs) | 320,735; 300,627 | 325,978; 316,647 |
| | | candidate | 29,440; 26,335 (34.0–38.0 µs) | 343,940; 344,691 | 358,577; 354,582 |
| Bun | stdio | base | 22,876; 24,118 (41.5–43.7 µs) | 290,983; 290,050 | 305,338; 299,152 |
| | | candidate | 25,310; 27,028 (37.0–39.5 µs) | 311,920; 319,728 | 332,998; 324,671 |
| Bun | TCP | base | 14,381; 13,394 (69.5–74.7 µs) | 280,649; 275,493 | 328,941; 325,337 |
| | | candidate | 15,952; 14,239 (62.7–70.2 µs) | 296,134; 278,697 | 364,815; 338,297 |
| Python asyncio | named pipe | base | 14,787; 16,326 (61.3–67.6 µs) | 193,458; 201,704 | 213,436; 226,568 |
| | | candidate | 21,448; 19,529 (46.6–51.2 µs) | 201,129; 217,517 | 228,436; 226,793 |
| Python asyncio | stdio | base | 13,352; 12,647 (74.9–79.1 µs) | 167,328; 163,396 | 199,621; 202,288 |
| | | candidate | 16,359; 17,251 (58.0–61.1 µs) | 176,342; 166,618 | 206,618; 199,353 |
| Python asyncio | TCP | base | 10,997; 11,300 (88.5–90.9 µs) | 160,302; 168,706 | 209,202; 207,613 |
| | | candidate | 11,674; 11,594 (85.7–86.3 µs) | 179,104; 174,029 | 216,432; 214,494 |

- **At depth the candidate serves 6 to 18 % more to Go** through every
  transport, from 292,888 to 325,378 before to 334,898 to 367,834, and 0 to
  15 % more to Bun, whose TCP at 64 is within its passes. Python asyncio gains
  −2 to 12 %: its own loop is its ceiling, as before.
- **One call in flight is shorter through the pipe for every client**, 34.0
  to 51.2 µs against 44.5 to 67.6, and through stdio for Bun and Python; the
  other cases' passes overlap between the builds.

## Sets

`Set`s a second, from Go; each is a durable commit.

| | 1 | 64 | 256 |
|---|---:|---:|---:|
| named pipe, base | 638; 615 | 13,857; 13,695 | 27,250; 26,362 |
| named pipe, candidate | 654; 627 | 15,204; 15,236 | 33,816; 35,529 |
| stdio, base | 633; 645 | 13,800; 13,902 | 26,241; 26,629 |
| stdio, candidate | 646; 657 | 15,696; 15,488 | 33,699; 34,565 |
| TCP, base | 648; 612 | 14,020; 13,493 | 28,346; 26,273 |
| TCP, candidate | 621; 639 | 15,319; 14,980 | 34,664; 33,541 |

- **At 256 in flight 18 to 35 % more sets, at 64 7 to 14 %; one in flight the
  same.** A set in a group ran three statements, its savepoint, its write and
  the savepoint's release, and each started the driver's goroutine: the
  savepoint's for the hold's deadline, the write's for its caller's context.
  The candidate starts none. The writer is one connection, so where a group
  is large its CPU a statement is the rate; that this is the whole of the
  difference is a hypothesis, as the candidate also reads by key without a
  goroutine where a set reads.

## Against the prototype's best sidecar

256 `Get`s in flight, the server's process profiled as `tinystore serve
--local` runs it, GOGC 400 included, through the directory's own transport;
beside it the round's sidecars through a Unix socket, `TestRPCServerCost`, in
the same session, their rates from its sweep and their CPU from its profiled
runs:

| | `Get`s a second | a `Get` allocates | collections in 2 s | CPU a `Get` | processors used |
|---|---:|---:|---:|---:|---:|
| base server | 321,283; 321,424 | 68.2 times, 3,340–3,341 bytes | 95; 95 | 33.7–34.5 µs | 10.8–11.1 |
| candidate server | 357,387; 353,448 | 59.2 times, 2,631 bytes | 87; 85 | 24.6–25.9 µs | 8.8–9.2 |
| candidate server, beside the prototype | 364,930; 365,792 | 59.2 times, 2,630–2,632 bytes | 92; 91 | 24.0–25.9 µs | 8.8–9.5 |
| prototype, a goroutine a call | 242,903; 247,542 | 54.2 times, 2,171 bytes | 895; 929 | 26.1–26.4 µs | 7.5–7.6 |
| prototype, 256 workers, GOGC 400 | 435,930; 421,049 | 52.1 times, 2,051–2,053 bytes | 153; 141 | 18.1 µs | 7.4 |

- **The server serves 84 to 87 % of the prototype's best**, 364,930 and
  365,792 gets a second against 435,930 and 421,049, where the report before
  measured 72 to 77 % in its own session; the prototype's rates here are the
  same machine's on the same day.
- **A get costs the server 24.0 to 25.9 µs of CPU**, 1.3 to 1.4 times the
  prototype's 18.1, where the base spent 33.7 to 34.5 in this session. The
  scheduler's spinning, `runtime.osyield`, fell from 13.1 to 15.4 % of the
  base's samples to 2.6 to 3.4 %, the prototype's best being 1.3 to 1.6.
- **What remains is allocation**: 59.2 a get against 52.1, and 2,631 bytes
  against 2,053. The candidate allocates 9 fewer and 709 bytes fewer than the
  base: by the code, what the two goroutines and their contexts took.

## What follows

- **The container's figures** and the race suite in it wait for Docker, which
  was not running; the report before found the container's numbers following
  Windows' by cause, not by size, so they are not inferred from these.
- **The server's own allocations**, seven a get more than the prototype's best
  sidecar: each stream's context, the entry's version and the answer's frame
  are where a profile of allocations begins, as the report before said.
- **What a call's own context still costs at depth, embedded**, 10 to 18 %:
  measured with contexts that share no parent before a claim is made about
  why.
- What this could not do: the container, macOS, bare Linux and a network
  between machines.
