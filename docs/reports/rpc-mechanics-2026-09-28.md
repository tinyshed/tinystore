# What reaching a sidecar costs — 2026-09-28

The round [docs/server.md](../server.md) asks for before `server/wire` is
built. Nothing here is the server: it is the prototype `spike/rpc_*`, a
sidecar holding one kv bucket of 10,000 keys of 100 bytes, reached through
frames laid out as [docs/wire.md](../wire.md) lays them out, from a Go client
inside the test and from the Bun and Python clients in `spike/testdata/rpc/`.
It repeats the probe that docs/server.md quoted, which ran outside the
repository, and answers what the probe left: each transport from each client,
who writes a connection's frames, what a call costs the sidecar, the credit a
stream needs, and a Python client without asyncio.

| Question | Answer | Decision |
|---|---|---|
| the local transport | on Windows a named pipe is the fastest for Go, Bun and asyncio Python alike: a `Get` in 31.1 to 32.0 µs from Go and Bun and 42.6 to 42.9 from Python, against 35.4 to 40.2 through a Unix socket or stdio and 55.0 to 70.2 through TCP; in the container a Unix socket and stdio carry 1.14 to 1.16 million echoes a second from Go, TCP 0.65 million | Windows: a named pipe; Linux and macOS: a Unix socket; stdio for a private child; TCP only between machines |
| calls in flight | one in flight is a round trip a call; at 256 every client meets the sidecar's own ceiling, 239,467 to 275,520 `Get`s a second on Windows and 194,534 to 222,202 in the container | every call a stream; an SDK's calls return promises |
| writes | 256 `Set`s in flight through the sidecar commit 32,359 to 34,823 a second on Windows against 36,275 to 37,078 embedded, and 21,720 to 23,334 in the container against 22,612 to 23,271 | the group hides the wire; the server runs calls concurrently |
| who writes a connection's frames | each sender for itself loses 3 to 27 times at depth; a goroutine of their own matches the first sender at depth and costs 2 to 18 µs a `Get` at one in flight | the first sender to find nobody writing writes what is queued |
| what the sidecar's time goes to | a goroutine started for each call grows its stack on the way into SQLite every time, and the default GC target collects some 500 times a second, shrinking the stacks workers keep: 260,153 to 267,708 `Get`s at 256 in flight on Windows and 212,834 to 220,617 in the container; with 256 workers and GOGC 400, 453,423 to 456,905 and 414,741 to 416,474, what the embedded program serves | calls run on long-lived workers, as many as the streams in flight, and the server sets its own GC target |
| stream credit | a window of 256 KiB moves 3,198 to 5,100 MiB/s through a local transport on Windows and 927 to 1,554 in the container, whose round trip is longer; 1 MiB moves 3,252 to 5,533 and 1,004 to 2,777 | 1 MiB a stream |
| a Python client without asyncio | a thread a call in flight serves 27,344 to 48,771 calls a second on Windows and 7,792 to 9,483 in the container at any depth | a sync client for a sync program's one call at a time; asyncio for concurrency |
| one call in flight in the container | 82.5 to 189.7 µs from every client, two to seven and a half times Windows; with every virtual processor kept busy, a Go echo through a socket takes 44.5 to 58.1 µs instead of 134.4 to 147.0 | a latency from WSL2 is not a Linux figure; bare Linux is measured before one is quoted |

## Environment and reproduction

- The prototype `spike/rpc_*` at `b670d6b`, over `2c9e15c`.
- **Windows.** AMD Ryzen 7 7700, 8 cores / 16 logical processors, 31.1 GiB,
  one Samsung SSD 990 PRO 1 TB (NVMe), Windows 11 Pro 10.0.26200 with NTFS and
  Defender's real-time protection on; go1.27.1 windows/amd64, Bun 1.4.2,
  CPython 3.13.15. The stores in the temporary directory on that NVMe.
- **The container**, on the same machine: Docker Desktop 29.6.2 on WSL2,
  kernel 6.18.33.2-microsoft-standard-WSL2, 16 CPUs and 15.2 GiB visible; the
  `golang:1.27` image, go1.27.1 linux/amd64 and its CPython 3.13.5; Bun 1.4.2
  copied from the `oven/bun:1.4.2` image onto the volume. The stores on a
  Docker volume, ext4 on the WSL2 virtual disk on the same NVMe.
- **The sidecar** is the test binary run again, `TestMain` taking the
  configuration from `TINYSTORE_RPC_SIDECAR`; its bucket is filled before the
  first case. A `Get` reads a random key, a `Set` writes a random key 100
  bytes, an echo returns 100 bytes without touching the store. One connection
  a case. The Bun and Python clients are built from the standard library and
  send raw bodies, so no codec is measured.
- **Two seconds a case, everything twice**; a range is the two passes. A
  latency is the calls in flight over the throughput, a mean: Go's clock on
  Windows advances in steps of about half a millisecond, so no percentile
  would be honest.
- **The order of the runs, 28 September, UTC.** Windows ran the five tests in
  one process, twice, from 12:53 to 13:08; the credit test again alone, twice,
  from 13:14 to 13:20, for the reason under [Credit](#credit). The container
  ran each test as a process of its own, twice, from 13:20 to 13:33; a
  `go vet` of this package ran on the host for a few seconds during its first
  pass. The sidecar's cost then gained its workers and GC variants and ran
  alone: on Windows once with GOGC 400 for the client as well (13:36), then
  twice as the table gives it (13:38 to 13:39); in the container twice, beside
  the one-call test idle and kept busy (13:39 to 13:45); then once each with
  the collector's stack shrinking off (13:45 to 13:47). The variants added
  options whose defaults are the code the earlier tests ran. The output is
  [data/rpc-mechanics-2026-09-28-windows.txt](data/rpc-mechanics-2026-09-28-windows.txt)
  and [data/rpc-mechanics-2026-09-28-linux.txt](data/rpc-mechanics-2026-09-28-linux.txt),
  their machine paths replaced.

```sh
# on Windows, from <repo>/spike, with bun and python on the path
TINYSTORE_SPIKE=1 TINYSTORE_RPC_SECONDS=2 go test . -run '^TestRPC' -v -count=1 -timeout 60m

# in the container: bun onto the volume, one binary, then each test a process of its own
docker run --rm -v <volume>:/perf oven/bun:1.4.2 sh -c 'mkdir -p /perf/bin && cp /usr/local/bin/bun /perf/bin/bun'
docker run --rm -v <repo>:/src -v <volume>:/perf -v <go cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off -e CGO_ENABLED=0 \
  -w /src golang:1.27 go test -c -o /perf/rpc-spike.test ./spike
docker run --rm -v <repo>:/src -v <volume>:/perf \
  -e TINYSTORE_SPIKE=1 -e TINYSTORE_RPC_SECONDS=2 -e TINYSTORE_RPC_DIR=/perf/rpc \
  -e PATH=/perf/bin:/go/bin:/usr/local/go/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  -w /src/spike golang:1.27 /perf/rpc-spike.test -test.run '^TestRPC<name>$' -test.v -test.count=1

# the one-call test with every virtual processor kept busy: the same run, its command given to sh -c as
#   for i in $(seq 16); do nice -n 19 sh -c 'while :; do :; done' & done; /perf/rpc-spike.test -test.run '^TestRPCOneInFlight$' -test.v
# the collector's stack shrinking off, on either: GODEBUG=gcshrinkstackoff=1 in the environment of the ServerCost run
```

## Transports

`Get`s a second through one connection; one in flight, the mean latency in
brackets. **Windows:**

| client | transport | 1 in flight | 64 | 256 |
|---|---|---:|---:|---:|
| Go, embedded | — | 155,562; 155,985 (6.4 µs) | 453,914; 465,963 | 403,076; 405,971 |
| Go | named pipe | 31,915; 32,148 (31.1–31.3 µs) | 269,275; 270,256 | 260,976; 265,816 |
| Go | Unix socket | 27,563; 28,046 (35.7–36.3 µs) | 266,253; 266,555 | 259,534; 263,605 |
| Go | stdio | 25,800; 26,266 (38.1–38.8 µs) | 280,300; 282,462 | 267,938; 272,793 |
| Go | TCP | 17,157; 17,284 (57.9–58.3 µs) | 269,734; 270,661 | 274,422; 274,444 |
| Bun | named pipe | 31,216; 31,371 (31.9–32.0 µs) | 237,117; 255,907 | 246,239; 249,290 |
| Bun | Unix socket | 26,805; 26,850 (37.2–37.3 µs) | 253,328; 254,596 | 258,672; 260,212 |
| Bun | stdio | 24,878; 26,224 (38.1–40.2 µs) | 258,014; 262,745 | 265,291; 265,558 |
| Bun | TCP | 17,014; 18,198 (55.0–58.8 µs) | 241,421; 249,735 | 263,438; 266,995 |
| Python asyncio | named pipe | 23,309; 23,465 (42.6–42.9 µs) | 206,250; 209,295 | 246,761; 257,749 |
| Python asyncio | stdio | 15,609; 17,036 (58.7–64.1 µs) | 195,669; 195,712 | 246,552; 255,994 |
| Python asyncio | TCP | 14,250; 14,820 (67.5–70.2 µs) | 178,978; 179,797 | 239,467; 239,660 |

**The container:**

| client | transport | 1 in flight | 64 | 256 |
|---|---|---:|---:|---:|
| Go, embedded | — | 159,793; 161,275 (6.2–6.3 µs) | 443,603; 451,822 | 324,336; 341,819 |
| Go | Unix socket | 5,502; 5,569 (179.6–181.8 µs) | 205,521; 210,062 | 214,911; 219,116 |
| Go | stdio | 6,363; 6,419 (155.8–157.2 µs) | 211,094; 213,035 | 212,224; 214,325 |
| Go | TCP | 5,755; 5,846 (171.1–173.8 µs) | 125,055; 126,203 | 194,534; 197,987 |
| Bun | Unix socket | 8,961; 9,192 (108.8–111.6 µs) | 204,074; 204,914 | 210,300; 212,015 |
| Bun | stdio | 7,092; 7,584 (131.9–141.0 µs) | 187,499; 191,380 | 216,423; 217,855 |
| Bun | TCP | 7,309; 7,448 (134.3–136.8 µs) | 141,592; 145,468 | 199,304; 201,605 |
| Python asyncio | Unix socket | 6,375; 6,524 (153.3–156.9 µs) | 201,335; 204,717 | 219,422; 221,186 |
| Python asyncio | stdio | 6,294; 6,381 (156.7–158.9 µs) | 167,869; 169,324 | 214,891; 222,202 |
| Python asyncio | TCP | 5,757; 6,319 (158.3–173.7 µs) | 134,474; 148,827 | 207,155; 212,167 |

Echoes, which touch no store, show what a transport itself carries: at one in
flight and at 256, a mean latency and a rate.

| client | transport | Windows, 1 | Windows, 256 | container, 1 | container, 256 |
|---|---|---:|---:|---:|---:|
| Go | named pipe | 16.1–16.3 µs | 1,486,823; 1,504,536 | — | — |
| Go | Unix socket | 19.6–20.3 µs | 1,456,312; 1,456,343 | 146.2–146.4 µs | 1,139,323; 1,151,410 |
| Go | stdio | 21.3–21.6 µs | 1,571,394; 1,584,520 | 120.8–123.4 µs | 1,149,441; 1,155,096 |
| Go | TCP | 30.7–31.7 µs | 1,482,434; 1,500,623 | 135.8–137.0 µs | 645,128; 648,591 |
| Bun | named pipe | 16.1–16.7 µs | 1,014,874; 1,031,701 | — | — |
| Bun | Unix socket | 19.0–19.1 µs | 1,044,729; 1,054,977 | 82.5–82.6 µs | 625,648; 635,973 |
| Bun | stdio | 19.7–20.1 µs | 1,002,591; 1,047,113 | 102.7–105.5 µs | 631,294; 650,922 |
| Bun | TCP | 29.5–30.1 µs | 960,621; 978,411 | 93.8–94.3 µs | 610,732; 616,742 |
| Python asyncio | named pipe | 26.9–27.0 µs | 511,334; 513,905 | — | — |
| Python asyncio | Unix socket | — | — | 153.4–154.1 µs | 456,856; 458,111 |
| Python asyncio | stdio | 35.0–35.7 µs | 471,642; 479,376 | 136.8–138.2 µs | 479,859; 497,674 |
| Python asyncio | TCP | 42.9–46.2 µs | 469,429; 482,796 | 151.8–155.0 µs | 494,244; 496,606 |

- **On Windows a named pipe is the fastest for every client.** Go reaches it
  through an overlapped handle that `os.NewFile` gives the runtime's
  completion port, Bun through `node:net`, Python's asyncio through its
  Proactor loop's `create_pipe_connection`. CPython on Windows has no
  `AF_UNIX`, and its asyncio no Unix connections. Bun reached a Unix socket
  whose path was short; the probe's path of 132 bytes, longer than
  `sockaddr_un` holds, did not connect.
- **One call in flight is a round trip, and 256 are the sidecar's ceiling.**
  From one to 64 in flight a connection serves 8 to 16 times the `Get`s on
  Windows and 19 to 38 times in the container; at 256, Go, Bun and Python
  serve within 15 % of each other, and what limits them is the sidecar,
  [below](#what-the-sidecars-time-goes-to).
- **TCP costs the most everywhere.** It is the slowest transport at one in
  flight on Windows, and in the container it carries 645,128 to 648,591
  echoes a second from Go where a Unix socket and stdio carry 1.14 to 1.16
  million.
- **In the container one call in flight takes 82.5 to 189.7 µs**, two to
  seven and a half times Windows, while an embedded `Get` takes the same 6.2 µs;
  [One call in flight in the container](#one-call-in-flight-in-the-container)
  tests why.

## Writes

`Set`s a second, from Go; each is a durable commit, `synchronous=FULL`.

| | Windows, 1 | 64 | 256 | container, 1 | 64 | 256 |
|---|---:|---:|---:|---:|---:|---:|
| embedded | 691; 706 | 19,978; 20,177 | 36,275; 37,078 | 361; 370 | 9,412; 9,503 | 22,612; 23,271 |
| named pipe | 678; 690 | 16,154; 16,212 | 33,550; 34,823 | — | — | — |
| Unix socket | 652; 665 | 15,822; 16,080 | 33,840; 34,331 | 331; 340 | 8,615; 8,874 | 22,346; 23,334 |
| stdio | 677; 687 | 16,346; 16,350 | 33,895; 34,241 | 345; 353 | 8,799; 8,896 | 21,720; 23,184 |
| TCP | 657; 664 | 15,327; 15,520 | 32,359; 33,999 | 344; 356 | 8,614; 9,309 | 22,930; 22,988 |

One `Set` in flight is one fsync wherever it comes from. At 64 in flight the
sidecar commits 76 to 99 % of the embedded rate, since a request on its way
misses the group that commits meanwhile; at 256, from 87 % to all of it.

## Who writes a connection's frames

From Go, each sender writing for itself (`naive`), a goroutine of their own
draining a queue into one buffered write (`goroutine`, the probe's), or the
first sender to find nobody writing, writing all that is queued as
`UpdateGrouped`'s leader commits (`leader`):

| where | writer | echo, 1 | echo, 64 | echo, 256 | `Get`, 1 | `Get`, 64 | `Get`, 256 |
|---|---|---:|---:|---:|---:|---:|---:|
| Windows, Unix socket | naive | 19.5–20.2 µs | 424,874; 427,811 | 439,726; 447,030 | 35.8–37.4 µs | 230,749; 234,666 | 244,589; 245,836 |
| | goroutine | 20.3–20.6 µs | 1,350,509; 1,363,370 | 1,402,990; 1,469,933 | 38.3–39.0 µs | 255,857; 260,119 | 259,468; 262,827 |
| | leader | 19.6–19.8 µs | 1,388,200; 1,388,361 | 1,472,200; 1,480,809 | 35.4–36.4 µs | 263,531; 269,066 | 256,744; 265,087 |
| Windows, TCP | naive | 31.5–31.6 µs | 97,253; 98,523 | 105,061; 105,906 | 57.8–59.4 µs | 79,459; 81,663 | 89,044; 93,072 |
| | goroutine | 31.5–33.0 µs | 825,994; 842,670 | 1,428,822; 1,453,606 | 61.8–64.5 µs | 259,178; 261,055 | 261,144; 265,836 |
| | leader | 31.8–32.5 µs | 895,540; 897,322 | 1,443,157; 1,479,139 | 57.1–58.0 µs | 262,900; 273,495 | 270,351; 275,520 |
| container, Unix socket | naive | 141.2–143.8 µs | 444,971; 450,131 | 481,021; 486,153 | 177.3–179.8 µs | 201,007; 214,916 | 211,509; 217,379 |
| | goroutine | 144.8–146.2 µs | 375,238; 394,063 | 1,082,822; 1,102,254 | 196.0–196.3 µs | 205,795; 205,871 | 212,605; 212,735 |
| | leader | 145.7–145.9 µs | 374,328; 374,991 | 1,107,763; 1,116,378 | 178.3–179.6 µs | 211,334; 214,320 | 213,819; 216,506 |
| container, TCP | naive | 131.3–132.7 µs | 23,808; 24,010 | 24,351; 24,851 | 171.4–172.4 µs | 24,369; 24,462 | 24,721; 25,818 |
| | goroutine | 135.9–138.5 µs | 326,967; 327,319 | 642,278; 650,685 | 184.9–185.5 µs | 124,312; 125,690 | 187,133; 191,220 |
| | leader | 135.2–135.6 µs | 326,137; 327,309 | 647,345; 652,393 | 171.9–175.5 µs | 126,439; 126,955 | 192,525; 195,821 |

- **A write a frame loses at depth**: 3.2 to 3.3 times fewer echoes through a
  Unix socket on Windows at 64 in flight, 9 to 14 times through TCP, 26 to 27
  in the container's TCP at 256.
- **A goroutine of their own costs a hand-off at one in flight**: 2 to 7 µs
  a `Get` on Windows, 9 to 18 µs in the container, since a sender wakes it
  rather than writing.
- **The first sender to find nobody writing** is as fast as a write a frame
  at one in flight and as fast as the goroutine at depth, with no goroutine to
  start and stop. Only the container's Unix socket at 64 echoes in flight
  favoured the write a frame, 444,971 to 450,131 against 374,328 to 374,991;
  its `Get`s did not.

## Credit

A 64 MiB transfer, again and again for two seconds, in 64 KiB frames, the
receiver granting what it consumed once half its window has gone; MiB a
second through windows of 64 KiB, 256 KiB, 1 MiB and 4 MiB. Each test ran as a
process of its own.

| where | transport | direction | 64 KiB | 256 KiB | 1 MiB | 4 MiB |
|---|---|---|---:|---:|---:|---:|
| Windows | named pipe | upload | 1,893; 1,938 | 3,571; 3,621 | 3,959; 4,024 | 3,935; 4,091 |
| | | download | 2,012; 2,015 | 3,933; 4,119 | 4,303; 4,305 | 4,325; 4,493 |
| | Unix socket | upload | 1,802; 1,838 | 3,930; 4,057 | 4,499; 4,570 | 4,439; 4,535 |
| | | download | 2,141; 2,191 | 5,097; 5,100 | 5,324; 5,533 | 5,425; 5,469 |
| | stdio | upload | 1,246; 1,276 | 3,198; 3,575 | 3,252; 3,538 | 3,209; 3,476 |
| | | download | 1,937; 2,045 | 4,190; 4,272 | 4,219; 4,281 | 4,196; 4,220 |
| | TCP | upload | 1,119; 1,126 | 1,422; 1,545 | 690; 728 | 500; 532 |
| | | download | 1,339; 1,344 | 1,741; 1,788 | 645; 765 | 695; 720 |
| container | Unix socket | upload | 347; 349 | 1,233; 1,257 | 1,962; 2,022 | 2,176; 2,194 |
| | | download | 413 | 1,545; 1,554 | 2,719; 2,777 | 3,562; 3,687 |
| | stdio | upload | 343; 346 | 927; 955 | 1,335; 1,439 | 1,533; 1,565 |
| | | download | 387; 408 | 1,111; 1,116 | 1,004; 1,036 | 994; 1,020 |
| | TCP | upload | 392; 394 | 1,226; 1,253 | 1,895; 1,965 | 2,610; 2,673 |
| | | download | 450; 454 | 1,446; 1,460 | 1,956; 1,961 | 2,203; 2,301 |

- **A window's worth is a round trip's.** With one 64 KiB frame a window, a
  transfer waits a round trip a frame; the container's round trip is several
  times Windows', and so is the window it needs: 256 KiB reaches 86 to 100 %
  of the best a local transport does on Windows, and 42 to 61 % through the
  container's Unix socket and its stdio uploads, where 1 MiB reaches 74 to
  93 %.
- **The engine is slower than the wire.** The blobs engine writes 0.55 to
  0.93 GB/s and reads 1.28 to 1.81 GB/s checked
  ([its round](blobs-engine-2026-09-27.md)); a local transport at 1 MiB passes
  both everywhere but the container's stdio.
- **TCP on Windows loses above 256 KiB**, in both directions, and so does a
  sender writing at most 256 KiB at a time: 595 to 628 MiB/s up at 1 MiB with
  that bound. What it loses is in how much is in flight, not in the size of a
  write; the design reaches a Windows sidecar through a named pipe.
- **Granting credit off the reading goroutine did not help**: downloads with
  the grant written by a goroutine of its own moved 1,908 to 1,969, 4,245 to
  4,279 and 4,924 to 4,958 MiB/s through a Windows Unix socket, below the
  reader's own grant at every window.
- **The first Windows passes ran this test after three others in one
  process**, and their downloads, received by that process, moved only 844 to
  1,468 MiB/s through a local transport where the process of its own moved
  1,937 to 5,533; their uploads, received by a sidecar started for the test,
  did not change. The data file keeps both; the table is the processes of
  their own.

## What the sidecar's time goes to

256 `Get`s in flight through a Unix socket for two seconds after one second of
them to warm the sidecar, then 256 `Set`s; the allocations a `Get` made in the
sidecar, from its `runtime.MemStats`. Six sidecars: a goroutine started for
each call; the same with bodies and answers from pools; 64 and 256 workers a
connection that take calls for as long as it lasts; and a goroutine a call and
256 workers with the sidecar's GOGC at 400. **Windows**, each test a process
of its own:

| sidecar | `Get`s a second | `Set`s a second | a `Get` allocates | collections in 2 s |
|---|---:|---:|---|---:|
| a goroutine a call | 260,153; 267,708 | 33,239; 34,161 | 52.21 times, 2,131 bytes | 963; 974 |
| the same, bodies and answers pooled | 306,014; 310,660 | 34,085; 34,169 | 49.26 times, 2,092–2,096 bytes | 662 |
| 64 workers | 332,282; 335,153 | 20,098; 20,189 | 50.15 times, 2,021 bytes | 992; 994 |
| 256 workers | 289,158; 292,026 | 33,237; 34,244 | 50.17 times, 2,023 bytes | 1,017; 1,045 |
| a goroutine a call, GOGC 400 | 394,544; 400,123 | 34,544; 35,200 | 52.11 times, 2,119 bytes | 129; 131 |
| 256 workers, GOGC 400 | 453,423; 456,905 | 34,693; 34,798 | 50.10 times, 2,014 bytes | 148; 152 |

**The container:**

| sidecar | `Get`s a second | `Set`s a second | a `Get` allocates | collections in 2 s |
|---|---:|---:|---|---:|
| a goroutine a call | 212,834; 220,617 | 21,698; 23,239 | 50.02 times, 2,021 bytes | 553; 582 |
| the same, bodies and answers pooled | 231,403; 235,924 | 22,637; 23,574 | 47.11 times, 1,985–1,987 bytes | 459; 461 |
| 64 workers | 312,875; 318,224 | 8,989; 9,331 | 47.98 times, 1,911 bytes | 628; 630 |
| 256 workers | 228,496; 231,180 | 22,021; 22,291 | 47.98 times, 1,912 bytes | 640; 652 |
| a goroutine a call, GOGC 400 | 294,001; 294,329 | 22,961; 23,150 | 49.99 times, 2,017 bytes | 87; 88 |
| 256 workers, GOGC 400 | 414,741; 416,474 | 22,456; 24,463 | 47.98 times, 1,911 bytes | 116; 117 |

A profile of the first sidecar and of the last, two seconds of 256 `Get`s in
flight, cumulative shares of the sidecar's samples:

| | Windows, a goroutine a call | Windows, 256 workers, GOGC 400 | container, a goroutine a call | container, 256 workers, GOGC 400 |
|---|---:|---:|---:|---:|
| processors the sidecar used | 7.4 to 7.5 | 7.5 to 7.6 | 6.7 | 6.9 |
| stacks growing, `runtime.newstack` | 11.1–12.1 % | 0.9–1.0 % | 26.2–26.4 % | 0.9–1.5 % |
| the runtime's locks, `runtime.semasleep` or `futex` | 15.4–15.7 % | 4.1–5.0 % | 3.5–4.4 % | 3.4–3.6 % |
| the scheduler, `runtime.findRunnable` | 11.0–11.7 % | 6.3–6.5 % | 5.6–6.4 % | 5.5–5.6 % |
| marking, `runtime.gcBgMarkWorker` | 1.4–1.5 % | at most 0.9 % | 2.8–3.5 % | 2.0 % |
| system calls, `cgocall` or `Syscall6` | 25.0–26.8 % | 32.6–36.0 % | 11.5–14.1 % | 14.2–15.9 % |
| SQLite, `sqlite3VdbeExec` | 23.2–23.3 % | 32.4–33.7 % | 30.3–31.5 % | 26.5–28.0 % |

- **A goroutine started for each call grows its stack every time.** The way
  into SQLite is deep, and a new goroutine starts small: in the container a
  quarter of the sidecar's time went on growing stacks, on Windows a ninth. A
  worker keeps its grown stack, and the profile's share falls to about 1 %.
- **The collector takes the workers' stacks back.** Each cycle may shrink a
  stack that uses little of itself, and at the default target a cycle ran
  about 500 times a second: 256 workers gained only 8 to 12 % on Windows and 4
  to 9 % in the container. With `GODEBUG=gcshrinkstackoff=1`, one pass each,
  the same 256 workers served 327,081 `Get`s on Windows and 321,596 in the
  container, 22 to 26 % and 46 to 51 % above a goroutine a call.
- **Fewer cycles cost the runtime less.** Marking itself took 1 to 4 % of the
  samples; what the cycles cost is the runtime's own locks and scheduling
  around them. At GOGC 400 a goroutine a call served 47 to 54 % more on Windows
  and 33 to 38 % more in the container, with 129 to 131 and 87 to 88
  collections in two seconds instead of 963 to 974 and 553 to 582. The one
  Windows run that set GOGC 400 for the client too served the same, 384,070 to
  402,203 a goroutine a call: the client's collector is not what limits it.
- **Together they reach the embedded program.** 256 workers at GOGC 400 served
  453,423 to 456,905 `Get`s on Windows and 414,741 to 416,474 in the
  container, against 403,076 to 405,971 and 324,336 to 341,819 embedded at 256
  goroutines and 453,914 to 465,963 and 443,603 to 451,822 at 64.
- **Too few workers starve the group.** 64 workers served the most `Get`s at
  the default target, and 20,098 to 20,189 `Set`s on Windows and 8,989 to
  9,331 in the container, 39 to 41 % and 58 to 60 % fewer than 256, since a
  group commits no more writes than there are workers to send them.
- **Pooling the frames saved three allocations a `Get`** of some fifty; the
  rest are kv's, `database/sql`'s and the driver's. What it gained, 14 to 19 %
  on Windows and 5 to 11 % in the container, came mostly from keeping more heap
  alive, which raised the collector's target: its collections sit between the
  default's and GOGC 400's.
- On Windows every `runtime.cgocall` sample came from `syscall.syscalln`, the
  socket's system calls and the driver's reads of pages alike, and every
  `runtime.semasleep` from `runtime.lock2`, the runtime's own locks.

## One call in flight in the container

From Go, `TestRPCOneInFlight`: one call at a time through each transport, as
the container runs it, then with sixteen `nice -n 19` busy loops beside it, one
a virtual processor, so that none of them goes idle:

| transport | echo, idle | echo, kept busy | `Get`, idle | `Get`, kept busy |
|---|---:|---:|---:|---:|
| Unix socket | 145.7–147.0 µs | 50.7–58.1 µs | 178.6–181.0 µs | 99.8–114.7 µs |
| stdio | 119.3–123.5 µs | 74.5–75.4 µs | 154.4–158.9 µs | 125.6–135.2 µs |
| TCP | 134.4–135.2 µs | 44.5–45.9 µs | 172.7–173.2 µs | 83.4–84.6 µs |

A round trip wakes each side once, and in the container waking an idle virtual
processor is most of it: kept busy, an echo through a socket takes a third to
two fifths as long. The container is WSL2's virtual machine, not a Linux host;
a sidecar's latency on bare Linux or macOS is measured before it is quoted.
From 64 calls in flight on, the rates above do not depend on it.

## Python without asyncio

A thread a call in flight and a reader thread answering them, with the
standard library's blocking sockets and pipes. `Get`s a second, the mean
latency at one in flight in brackets:

| where | transport | 1 in flight | 64 | 256 |
|---|---|---:|---:|---:|
| Windows | stdio | 21,669; 22,115 (45.2–46.1 µs) | 38,822; 39,795 | 38,452; 38,545 |
| | TCP | 12,699; 12,915 (77.4–78.7 µs) | 28,036; 28,409 | 27,344; 27,463 |
| container | Unix socket | 5,271; 5,284 (189.3–189.7 µs) | 8,769; 9,053 | 8,161; 8,510 |
| | stdio | 6,571; 6,590 (151.7–152.2 µs) | 8,362; 8,707 | 8,151; 8,325 |
| | TCP | 5,679; 5,875 (170.2–176.1 µs) | 8,152; 8,261 | 7,792; 7,933 |

One call at a time it is as fast as asyncio, and faster over stdio on
Windows, 45.2 to 46.1 µs against 58.7 to 64.1; more threads do not add calls,
since each call hands the interpreter's lock from thread to thread. A named
pipe needs overlapped I/O, which Python's `open` does not do: a pending read
on the handle it makes holds every write behind it, so the client skipped it.

## What the design takes from it

- **A local transport by platform**: a named pipe on Windows for every
  client, a Unix socket elsewhere for a shared sidecar, stdio for a private
  one; TCP between machines only. Windows no longer needs a loopback port and
  its token: the pipe's DACL is the permission.
- **The first sender writes**: no writer goroutine, and a write a frame only
  when nobody else is writing.
- **The server's calls run on long-lived workers, as many as the streams in
  flight**, not fewer, and the server sets its garbage collector's target
  rather than leaving it at the default; how, a GOGC or a limit from
  `Options.Memory`, is settled when the server is built, and measured then.
- **A stream's credit is 1 MiB** by default.
- **A Python client without asyncio** serves one call at a time; concurrency
  is asyncio's, and on Windows such a client reaches a pipe through
  overlapped I/O, as `multiprocessing` does.
- What this round could not do: macOS, bare Linux, a network between two
  machines, and the SDKs' own codecs.
