"""A Python client of `tinystore serve`, server/spike/serve_spike_test.go: frames
as docs/wire.md lays them out, their bytes the ones Go's wire wrote, given in
TINYSTORE_BENCH_FRAMES, so that no codec is measured; every call a stream of
its own, the frames asked for in a loop turn written at once, through asyncio.
Standard library only. It prints a JSON line a case."""

import asyncio
import json
import os
import random
import struct
import sys
import time

HEADER = struct.Struct("<IBBHI")
STREAM = struct.Struct("<I")
WELCOME, RESPONSE, GOAWAY = 2, 4, 10
ERROR = 2
KEYS = 10_000
DEPTHS = (1, 64, 256)
TRANSPORT = os.environ["TINYSTORE_BENCH_TRANSPORT"]
ADDRESS = os.environ.get("TINYSTORE_BENCH_ADDRESS", "")
SECONDS = float(os.environ.get("TINYSTORE_RPC_SECONDS", "2"))
GIVEN = json.loads(os.environ["TINYSTORE_BENCH_FRAMES"])
HELLO, OPEN, HANDLE, GET = (bytes.fromhex(GIVEN[name]) for name in ("hello", "open", "handle", "get"))
DIGITS = GIVEN["key"]
KEY_DIGITS = [f"{i:05d}".encode() for i in range(KEYS)]


def emit(**line):
    print(json.dumps(line), flush=True)


class Conn:
    def __init__(self, write):
        self.write = write
        self.pending = {}
        self.welcomed = None
        self.next = 1  # stream 1 is the open's
        self.out = []
        self.scheduled = False
        self.loop = asyncio.get_running_loop()
        self.buffer = bytearray()

    def feed(self, data):
        buffer = self.buffer
        buffer += data
        at, end = 0, len(buffer)
        while end - at >= 12:
            length, kind, flags, _, stream = HEADER.unpack_from(buffer, at)
            stop = at + 12 + length
            if stop > end:
                break
            if kind in (WELCOME, GOAWAY):
                self.welcomed.set_result(kind)
            elif kind == RESPONSE:  # CREDIT and PING need nothing of a case shorter than a silence
                future = self.pending.pop(stream, None)
                if future is not None:
                    future.set_result((flags, bytes(buffer[at + 12 : stop])))
            at = stop
        del buffer[:at]

    def send(self, frame):
        self.out.append(frame)
        if not self.scheduled:
            self.scheduled = True
            self.loop.call_soon(self.flush)

    def flush(self):
        self.scheduled = False
        self.write(b"".join(self.out))
        self.out.clear()

    async def handshake(self):
        self.welcomed = self.loop.create_future()
        self.send(HELLO)
        if await self.welcomed != WELCOME:
            raise RuntimeError("the server said GOAWAY")

    async def call(self, frame, stream):
        future = self.loop.create_future()
        self.pending[stream] = future
        self.send(frame)
        flags, body = await future
        if flags & ERROR:
            raise RuntimeError("the server answered an error")
        return body

    def get(self, k):
        self.next = (self.next + 1) & 0xFFFFFFFF or 2
        frame = bytearray(GET)
        STREAM.pack_into(frame, 8, self.next)
        frame[DIGITS : DIGITS + 5] = KEY_DIGITS[k]
        return self.call(bytes(frame), self.next)


class Feed(asyncio.Protocol):
    conn = None

    def data_received(self, data):
        self.conn.feed(data)


def found(body):
    if len(body) < 3 or body[1] != 1 or body[2] != 0xC3:
        raise RuntimeError("a key of the bucket was not found")


async def measure(conn):
    await conn.handshake()
    if await conn.call(OPEN, 1) != HANDLE:
        raise RuntimeError("kv.open answered another handle")
    for depth in DEPTHS:
        done = 0
        deadline = time.perf_counter() + SECONDS

        async def worker():
            nonlocal done
            rng = random.Random()
            while time.perf_counter() < deadline:
                found(await conn.get(rng.randrange(KEYS)))
                done += 1

        began = time.perf_counter()
        await asyncio.gather(*(worker() for _ in range(depth)))
        emit(op="get", depth=depth, ops=done / (time.perf_counter() - began))


async def main():
    loop = asyncio.get_running_loop()
    if TRANSPORT == "stdio":
        argv = json.loads(os.environ["TINYSTORE_BENCH_COMMAND"])
        sidecar = await asyncio.create_subprocess_exec(*argv, stdin=asyncio.subprocess.PIPE,
                                                       stdout=asyncio.subprocess.PIPE,
                                                       stderr=asyncio.subprocess.DEVNULL, limit=1 << 20)
        conn = Conn(sidecar.stdin.write)

        async def read():
            while data := await sidecar.stdout.read(1 << 16):
                conn.feed(data)

        reading = asyncio.create_task(read())
        await measure(conn)
        sidecar.stdin.close()
        await sidecar.wait()
        await reading
        return
    if TRANSPORT == "tcp":
        host, port = ADDRESS.rsplit(":", 1)
        transport, feed = await loop.create_connection(Feed, host, int(port))
    elif TRANSPORT == "unix":
        if not hasattr(loop, "create_unix_connection") or sys.platform == "win32":
            emit(skipped="asyncio has no Unix connections on this platform")
            return
        transport, feed = await loop.create_unix_connection(Feed, ADDRESS)
    elif TRANSPORT == "pipe":
        transport, feed = await loop.create_pipe_connection(Feed, ADDRESS)
    else:
        emit(skipped=f"no transport {TRANSPORT}")
        return
    feed.conn = Conn(transport.write)
    await measure(feed.conn)
    transport.close()


if __name__ == "__main__":
    asyncio.run(main())
