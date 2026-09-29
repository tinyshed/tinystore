# The built server against its prototype — 2026-09-29

The measurement [docs/server.md](../server.md) asks for once `tinystore serve`
exists: the cases of [the round](rpc-mechanics-2026-09-28.md) through the built
server, on the same machine, beside the prototype run again in the same
session. `server/spike` builds `tinystore` from `cmd/tinystore`, fills the
round's bucket, 10,000 keys of 100 bytes, as a program embedding the store
would, starts `tinystore serve` on it, and calls `kv.get` of a random key and
`kv.set` of one from Go (`server/internal/client`), Bun and Python asyncio,
whose clients send the frames Go's wire wrote so that no codec of theirs is
measured, at 1, 64 and 256 in flight through one connection: the directory's
own transport, a named pipe on Windows and a Unix socket in the container,
found through `SERVE` and its proof; a private child's stdio; TCP with a
token. The prototype `spike/rpc_*` ran `TestRPCEmbedded`, `TestRPCTransports`
and `TestRPCServerCost` beside it, pass for pass.

| Question | Answer | What follows |
|---|---|---|
| gets at 64 and 256 in flight | Windows, Go and Bun: 267,159 to 331,048 a second through the server, 149,954 to 268,227 through the prototype as its round ran it, a goroutine a call at the default GC target; the container: 118,285 to 273,256 against 110,264 to 205,031 | the server's workers and GOGC 400 pay as the round said they would |
| against the prototype's best sidecar | 256 workers at GOGC 400 served 404,799 to 430,757 on Windows and 347,411 to 372,521 in the container; the server, through the directory's own transport, 309,955 to 311,446 and 254,102 to 272,446, 72 to 77 % and 68 to 78 % of it, spending 36.3 to 37.2 µs of CPU a get on Windows and 28.1 to 29.2 in the container against 17.7 to 18.3 | a rate the server does not reach: [below](#against-the-prototypes-best-sidecar) |
| why | the largest cause found: a point read whose context can end costs a goroutine in `database/sql`, which watches every such query's context: embedded, with a call's own context, 27 to 35 % fewer gets at 64 and 256 in flight on both platforms, and a get at one in flight 7.3 to 8.1 µs slower on Windows; the server hands kv each call's own context | point reads run their statement without cancellation, after checking the context: [What follows](#what-follows) |
| one call in flight | as the prototype, within what two passes of either differ: 12,836 to 25,685 gets a second on Windows against 12,839 to 22,686, 5,123 to 8,035 in the container against 4,639 to 8,578 | a latency is still the round trip's and the machine's |
| sets | the group commit hides the wire as before: at 256, 25,903 to 31,810 a second on Windows against the prototype's 27,486 to 28,521 and 30,327 to 31,521 embedded; in the container 20,699 to 21,818 against 20,765 to 21,954; one in flight there 281 to 305 against 323 to 345, about 12 % fewer | the container's one set in flight is a finding with a hypothesis, [Sets](#sets) |

## Environment and reproduction

- `tinystore serve`, the server and `server/spike` at `1da0bd7`; the profile at
  `46365e6` and the context's measurement at `1885baf`, whose server and
  engines are `1da0bd7`'s. The prototype `spike/rpc_*` as the round committed
  it at `b670d6b`, built with the engines of `1da0bd7`.
- **Windows.** AMD Ryzen 7 7700, 8 cores / 16 logical processors, 31.1 GiB,
  one Samsung SSD 990 PRO 1 TB (NVMe), Windows 11 Pro 10.0.26200 with NTFS and
  Defender's real-time protection on; go1.27.1 windows/amd64, Bun 1.4.2,
  CPython 3.13.15. The stores in the temporary directory on that NVMe.
- **The container**, on the same machine: Docker Desktop 29.6.2 on WSL2, kernel
  6.18.33.2-microsoft-standard-WSL2, 16 CPUs visible; the `golang:1.27` image,
  go1.27.1 linux/amd64 and its CPython 3.13.5; Bun 1.4.2 copied from the
  `oven/bun:1.4.2` image onto the volume, as the round did. The stores on a
  Docker volume, ext4 on the WSL2 virtual disk on the same NVMe.
- **Two seconds a case, everything twice**, a range the two passes; a latency
  is the calls in flight over the rate, a mean, as the round gives it. The
  server's bucket is filled and closed before `tinystore serve` opens it; the
  prototype's sidecar fills its own and serves it open.
- **The order of the runs, 28 September, UTC** (29 September, UTC+3, on the
  machine's clock). Windows: prototype, server, prototype, server, the
  prototype's three tests one process and the server's another, from 22:28 to
  22:40; then two profiles of the server, 22:51 to 22:52, and two passes of the
  context's measurement, 22:53 to 22:54. The container: prototype and server
  alternating, each test a process of its own, from 22:40 to 22:51; then the
  profile and the context's measurement, twice, 22:54 to 22:55. Nothing ran
  beside them but reading their logs and writing files. The output
  is [data/rpc-server-2026-09-29-windows.txt](data/rpc-server-2026-09-29-windows.txt)
  and [data/rpc-server-2026-09-29-linux.txt](data/rpc-server-2026-09-29-linux.txt),
  the user's temporary directory replaced by `<tmp>`.

```sh
# on Windows, from <repo>, with bun and python on the path; each pass twice
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 go test ./spike -run '^TestRPC(Embedded|Transports|ServerCost)$' -v -count=1 -timeout 60m
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 go -C server test ./spike -run '^TestServeTransports$' -v -count=1 -timeout 60m
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 go -C server test ./spike -run '^TestServeProfile$' -v -count=1
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 go -C server test ./spike -run '^TestGetsWithAContextThatCanEnd$' -v -count=1

# in the container: bun on the volume as the round put it there, the binaries built once, each test a process
docker run --rm -v <repo>:/src -v <volume>:/perf -v <go cache>:/go \
  -e GOWORK=off -e CGO_ENABLED=0 -e TINYSTORE_SPIKE=1 -e TINYSTORE_RPC_SECONDS=2 -e TINYSTORE_RPC_DIR=/perf/rpc \
  -e PATH=/perf/bin:/go/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  -w /src golang:1.27 sh -c 'go test -c -o /perf/rpc-spike.test ./spike && go -C server test -c -o /perf/serve-spike.test ./spike'
# then each test a process of its own, twice, the prototype's and the server's alternating: the same docker run
#   with -w /src/spike         /perf/rpc-spike.test -test.run '^TestRPC<name>$' -test.v -test.count=1
#   with -w /src/server/spike  /perf/serve-spike.test -test.run '^Test<name>$' -test.v -test.count=1
```

## Gets

`Get`s a second through one connection, the mean latency at one in flight in
brackets. **Windows:**

| client | transport | | 1 in flight | 64 | 256 |
|---|---|---|---:|---:|---:|
| Go, embedded | — | | 146,675; 148,882 (6.7–6.8 µs) | 372,107; 389,432 | 326,406; 341,239 |
| Go | named pipe | prototype | 22,310; 16,397 (44.8–61.0 µs) | 239,995; 163,485 | 237,268; 149,954 |
| | | server | 18,838; 20,441 (48.9–53.1 µs) | 291,377; 291,095 | 296,539; 296,937 |
| Go | stdio | prototype | 20,012; 21,404 (46.7–50.0 µs) | 253,034; 268,227 | 242,275; 262,212 |
| | | server | 23,206; 20,586 (43.1–48.6 µs) | 319,759; 309,773 | 311,324; 328,705 |
| Go | TCP | prototype | 13,329; 13,108 (75.0–76.3 µs) | 228,840; 254,312 | 234,706; 256,400 |
| | | server | 12,836; 13,427 (74.5–77.9 µs) | 295,818; 299,467 | 322,296; 294,956 |
| Bun | named pipe | prototype | 20,443; 19,284 (48.9–51.9 µs) | 213,932; 215,713 | 221,259; 220,918 |
| | | server | 25,685; 24,936 (38.9–40.1 µs) | 311,970; 307,078 | 306,287; 325,129 |
| Bun | stdio | prototype | 18,930; 19,999 (50.0–52.8 µs) | 230,596; 222,583 | 229,641; 246,532 |
| | | server | 24,417; 22,839 (41.0–43.8 µs) | 288,651; 282,726 | 295,451; 305,010 |
| Bun | TCP | prototype | 12,839; 13,455 (74.3–77.9 µs) | 211,604; 217,985 | 234,775; 243,080 |
| | | server | 13,281; 15,528 (64.4–75.3 µs) | 267,159; 279,041 | 326,953; 331,048 |
| Python asyncio | named pipe | prototype | 13,700; 20,402 (49.0–73.0 µs) | 162,440; 202,517 | 215,895; 234,938 |
| | | server | 18,211; 16,711 (54.9–59.8 µs) | 181,904; 200,037 | 220,472; 222,831 |
| Python asyncio | stdio | prototype | 11,613; 15,417 (64.9–86.1 µs) | 146,565; 188,743 | 210,444; 235,782 |
| | | server | 12,562; 13,926 (71.8–79.6 µs) | 158,895; 175,525 | 193,649; 214,762 |
| Python asyncio | TCP | prototype | 9,133; 9,649 (103.6–109.5 µs) | 142,186; 153,151 | 211,851; 219,225 |
| | | server | 11,244; 10,976 (88.9–91.1 µs) | 167,746; 156,970 | 212,625; 213,412 |

The prototype also served its Unix socket, which the server leaves to Windows
programs of its own: Go 19,955 to 22,686 at one in flight and 233,435 to
253,235 at 64 and 256, Bun 18,279 to 19,691 and 217,854 to 234,425.
**The container:**

| client | transport | | 1 in flight | 64 | 256 |
|---|---|---|---:|---:|---:|
| Go, embedded | — | | 145,727; 147,600 (6.8–6.9 µs) | 400,930; 414,228 | 294,811; 308,580 |
| Go | Unix socket | prototype | 4,639; 5,388 (185.6–215.6 µs) | 159,678; 199,884 | 195,799; 196,303 |
| | | server | 5,318; 5,123 (188.0–195.2 µs) | 225,186; 173,805 | 254,367; 208,628 |
| Go | stdio | prototype | 6,049; 6,009 (165.3–166.4 µs) | 188,597; 183,130 | 198,706; 190,036 |
| | | server | 6,787; 6,768 (147.3–147.8 µs) | 242,528; 225,955 | 252,546; 273,256 |
| Go | TCP | prototype | 5,544; 5,572 (179.5–180.4 µs) | 110,264; 119,487 | 130,622; 180,144 |
| | | server | 5,724; 5,637 (174.7–177.4 µs) | 125,798; 118,285 | 214,597; 192,615 |
| Bun | Unix socket | prototype | 8,578; 8,034 (116.6–124.5 µs) | 193,956; 175,520 | 205,031; 192,601 |
| | | server | 8,035; 7,568 (124.4–132.1 µs) | 219,583; 192,881 | 238,716; 210,599 |
| Bun | stdio | prototype | 6,878; 6,938 (144.1–145.4 µs) | 179,997; 178,652 | 199,612; 199,961 |
| | | server | 8,018; 7,977 (124.7–125.4 µs) | 205,583; 190,131 | 233,639; 225,369 |
| Bun | TCP | prototype | 7,102; 6,955 (140.8–143.8 µs) | 133,614; 135,242 | 180,917; 190,546 |
| | | server | 7,045; 7,158 (139.7–142.0 µs) | 163,781; 165,063 | 236,253; 224,880 |
| Python asyncio | Unix socket | prototype | 5,275; 5,079 (189.6–196.9 µs) | 164,524; 156,143 | 198,725; 214,936 |
| | | server | 5,264; 5,094 (190.0–196.3 µs) | 174,346; 161,277 | 219,735; 184,251 |
| Python asyncio | stdio | prototype | 5,968; 5,345 (167.6–187.1 µs) | 157,504; 128,874 | 205,837; 213,459 |
| | | server | 6,181; 6,059 (161.8–165.0 µs) | 142,089; 138,207 | 225,016; 210,600 |
| Python asyncio | TCP | prototype | 5,428; 5,286 (184.2–189.2 µs) | 126,495; 126,861 | 196,085; 184,327 |
| | | server | 5,454; 5,441 (183.4–183.8 µs) | 145,522; 140,869 | 217,236; 193,561 |

- **The machine is not the round's.** The same prototype on the same machine
  served a `Get` through a Unix socket on Windows in 44.1 to 50.1 µs where the
  round measured 35.7 to 36.3, and the embedded program 372,107 to 389,432 at
  64 where it measured 453,914 to 465,963. So nothing here is compared with the
  round's figures, only with the prototype run beside it.
- **At depth the server serves more than the prototype as the round ran it**,
  a goroutine a call at the default GC target: from Go and Bun 267,159 to
  331,048 a second on Windows against 149,954 to 268,227, and 118,285 to
  273,256 in the container against 110,264 to 205,031. Python asyncio is
  within what its two passes differ, 156,970 to 222,831 against 142,186 to
  235,782 on Windows: its own loop is its ceiling.
- **One call in flight is the round trip's**: the server's latency is the
  prototype's within the spread of two passes, 18.8 to 25.7 thousand a second
  from Go and Bun through a Windows pipe against 16.4 to 22.3. Bun reached
  the server faster than the prototype through the pipe and stdio, 38.9 to
  43.8 µs against 48.9 to 52.8; its clients differ as much as the servers do.

## Sets

`Set`s a second, from Go; each is a durable commit.

| | Windows, 1 | 64 | 256 | container, 1 | 64 | 256 |
|---|---:|---:|---:|---:|---:|---:|
| embedded | 688; 676 | 18,257; 18,235 | 31,521; 30,327 | 354; 362 | 8,891; 9,212 | 22,460; 22,612 |
| named pipe or Unix socket, prototype | 674; 588 | 14,240; 15,174 | 27,916; 27,827 | 336; 326 | 8,675; 8,543 | 21,954; 21,809 |
| named pipe or Unix socket, server | 657; 663 | 14,885; 15,125 | 31,666; 31,014 | 286; 281 | 8,346; 7,794 | 20,699; 20,830 |
| stdio, prototype | 666; 667 | 14,634; 14,769 | 27,602; 27,902 | 345; 328 | 8,623; 8,228 | 21,170; 21,949 |
| stdio, server | 638; 658 | 14,026; 15,216 | 26,347; 31,810 | 294; 305 | 8,418; 8,283 | 21,702; 20,938 |
| TCP, prototype | 631; 648 | 13,833; 13,916 | 27,653; 28,521 | 329; 323 | 8,912; 8,176 | 21,815; 20,765 |
| TCP, server | 640; 638 | 13,320; 14,737 | 25,903; 31,727 | 285; 299 | 8,431; 8,168 | 21,242; 21,818 |

- **At 64 and 256 a set is the disk's and the group's**, through either and
  embedded alike.
- **One set in flight in the container is about 12 % slower through the
  server**, 281 to 305 a second against the prototype's 323 to 345, where on
  Windows the two are the same. The one difference in how they begin is that
  the server opens a bucket filled and closed, whose write-ahead log starts
  empty and grows under these sets, where the prototype's sidecar writes into
  the log its filling left; that a growing log's fsync costs more on ext4 is a
  hypothesis, which no measurement here separates from the alternatives.

## Against the prototype's best sidecar

256 `Get`s in flight after a second of them to warm the server, a CPU profile
of the server's process meanwhile, and what it allocated: `TestServeProfile`,
the server serving its directory as `tinystore serve --local` does, GOGC 400
included, through the directory's own transport. Beside it the round's
sidecars in the same session, `TestRPCServerCost`, through a Unix socket:

| | `Get`s a second | a `Get` allocates | collections in 2 s | CPU a `Get` | processors used |
|---|---:|---:|---:|---:|---:|
| Windows, prototype, a goroutine a call | 247,672; 255,930 | 52.2 times, 2,130 bytes | 891; 956 | 23.5–25.1 µs | 7.2–7.4 |
| Windows, prototype, 256 workers, GOGC 400 | 430,757; 404,799 | 50.1 times, 2,014 bytes | 127; 138 | 17.7–17.8 µs | 7.2–7.4 |
| Windows, the server | 311,446; 309,955 | 68.1 times, 3,332–3,340 bytes | 89; 91 | 36.3–37.2 µs | 11.3–11.5 |
| container, prototype, a goroutine a call | 192,056; 200,262 | 50.0 times, 2,021 bytes | 496; 514 | 30.7–33.6 µs | 6.7–6.8 |
| container, prototype, 256 workers, GOGC 400 | 347,411; 372,521 | 48.0 times, 1,911 bytes | 97; 106 | 17.8–18.3 µs | 6.8–6.9 |
| container, the server | 254,102; 272,446 | 64.7 times, 3,158 bytes | 72; 78 | 28.1–29.2 µs | 7.4–7.7 |

- **The server spends twice the prototype's CPU on a `Get`**, and on Windows
  half again the processors: 11.3 to 11.5 against 7.2 to 7.4. Its profile
  there puts 14.4 to 16.7 % of the samples in `runtime.osyield`, the scheduler
  spinning while it steals work, where the prototype's best puts 1.1 to 1.9 %;
  `runtime.findRunnable` takes 21.6 % cumulative, where the round's profile of
  the best sidecar put it at 6.3 to 6.5 %.
- **Inside the engine the same read costs more.** In the container's second
  profile kv's `GetEntry` took 61.6 % of 15.30 s for 544,892 `Get`s, 17.3 µs
  each, about what the whole of the prototype's best sidecar spends on one;
  the same function, called with `context.Background` there and with each
  call's own context here.
- **What the server adds around it**, in the container: answering, `respond`,
  9.4 % of the samples, most of it the connection's writer, 8.0 %; the
  scheduler 9.8 %. A `Get` allocates 17 to 18 more times than in the
  prototype's best sidecar, and 1.2 to 1.3 KB more.

## A context that can end

`TestGetsWithAContextThatCanEnd`, no server: the round's `Get`s on the
embedded bucket at 1, 64 and 256 in flight, once with `context.Background` and once
with each call's own `context.WithCancel` of a longer-lived one, as the server
derives a stream's from its connection's. `database/sql` watches the context
of every query whose context can end from a goroutine of its own
(`Rows.initContextClose` in Go 1.27.1), which `context.Background` cannot.

| | Windows, 1 | 64 | 256 | container, 1 | 64 | 256 |
|---|---:|---:|---:|---:|---:|---:|
| `context.Background` | 120,517; 123,751 (8.1–8.3 µs) | 348,369; 345,926 | 314,470; 299,507 | 129,418; 135,397 (7.4–7.7 µs) | 399,910; 388,034 | 313,972; 296,156 |
| a call's own, which can end | 64,217; 61,570 (15.6–16.2 µs) | 252,018; 251,289 | 205,963; 210,549 | 117,081; 115,402 (8.5–8.7 µs) | 265,636; 252,798 | 203,058; 195,307 |

- **A point read whose context can end costs a goroutine**: 27 to 28 % fewer
  `Get`s at 64 in flight on Windows and 30 to 35 % at 256; 34 to 35 % at both
  in the container; on Windows a `Get` at one in flight takes 7.3 to 8.1 µs
  more, nearly twice the embedded read, and in the container 0.8 to 1.3 µs
  more.
- **It is every program's, not the server's.** A Go program passing a request's
  context to kv pays it as the server does; every embedded figure measured so
  far passed `context.Background`.

## What follows

- **Point reads run their statement without cancellation.** `internal/sqlite`'s
  `QueryRow`, the one-row read every engine's lookups go through, checks the
  context first and then runs the statement with `context.WithoutCancel`, so
  that `database/sql` starts no goroutine: a point read takes microseconds,
  and its wait for a reader slot still honours the context. A scan and a
  query an application writes keep theirs, since those are what a deadline
  interrupts. Measured again beside this report's figures once built.
  Built as `sqlite.QueryRowByKey`, with a grouped write's statements run
  without their caller's context too, and measured in
  [rpc-contexts-2026-09-29](rpc-contexts-2026-09-29.md): the server at 84 to
  87 % of the prototype's best in that session.
- **The server's own allocations** are what remains between it and the
  prototype's best, about a third of a `Get`'s CPU in the container: each
  stream's context, the entry's version and the answer's frame are where a
  profile of allocations begins.
- **The container's one set in flight** is measured again with a bucket the
  server fills itself before a claim is made about why.
- What this could not do: macOS, bare Linux, and a network between machines.
