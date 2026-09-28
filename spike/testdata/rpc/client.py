"""A Python client of the rpc round's sidecar, spike/rpc_spike_test.go: frames as
docs/wire.md lays them out, every call a stream of its own. Two ways:

    client.py asyncio   one event loop; the frames asked for in a loop turn written at once
    client.py threads   a thread a call in flight, a reader thread answering them

Standard library only: the bodies are raw bytes, so no codec is measured. It
prints a JSON line a case."""

import asyncio
import json
import os
import random
import socket
import struct
import subprocess
import sys
import threading
import time

HEADER = struct.Struct("<IBBHI")
REQUEST, END = 3, 1
ECHO, GET = 1, 2
KEYS = 10_000
DEPTHS = (1, 64, 256)
TRANSPORT = os.environ["TINYSTORE_RPC_TRANSPORT"]
ADDRESS = os.environ.get("TINYSTORE_RPC_ADDRESS", "")
SECONDS = float(os.environ.get("TINYSTORE_RPC_SECONDS", "2"))
VALUE = bytes([7]) * 100
KEY_BYTES = [f"k{i:05d}".encode() for i in range(KEYS)]


def emit(**line):
    print(json.dumps(line), flush=True)


def sidecar_command():
    argv = json.loads(os.environ["TINYSTORE_RPC_SIDECAR_COMMAND"])
    env = dict(os.environ)
    env[os.environ["TINYSTORE_RPC_SIDECAR_VARIABLE"]] = os.environ["TINYSTORE_RPC_SIDECAR_CONFIG"]
    return argv, env


class Frames:
    """Cuts a byte stream into frames and hands each answer to its stream."""

    def __init__(self, answered):
        self.buffer = bytearray()
        self.answered = answered

    def feed(self, data):
        buffer = self.buffer
        buffer += data
        at, end = 0, len(buffer)
        while end - at >= 12:
            length, _, _, _, stream = HEADER.unpack_from(buffer, at)
            stop = at + 12 + length
            if stop > end:
                break
            self.answered(stream, bytes(buffer[at + 12 : stop]))
            at = stop
        del buffer[:at]


class AsyncConn:
    def __init__(self, write):
        self.write = write
        self.pending = {}
        self.next = 0
        self.out = []
        self.scheduled = False
        self.loop = asyncio.get_running_loop()
        self.frames = Frames(self.answered)

    def answered(self, stream, body):
        future = self.pending.pop(stream, None)
        if future is not None:
            future.set_result(body)

    def call(self, method, body):
        self.next = (self.next + 1) & 0xFFFFFFFF
        future = self.loop.create_future()
        self.pending[self.next] = future
        self.out.append(HEADER.pack(len(body), REQUEST, END, method, self.next) + body)
        if not self.scheduled:
            self.scheduled = True
            self.loop.call_soon(self.flush)
        return future

    def flush(self):
        self.scheduled = False
        self.write(b"".join(self.out))
        self.out.clear()


class Feed(asyncio.Protocol):
    conn = None

    def data_received(self, data):
        self.conn.frames.feed(data)


async def measure_async(conn):
    await conn.call(ECHO, VALUE)  # the sidecar has filled its bucket
    for method, op in ((ECHO, "echo"), (GET, "get")):
        for depth in DEPTHS:
            done = 0
            deadline = time.perf_counter() + SECONDS

            async def worker():
                nonlocal done
                rng = random.Random()
                while time.perf_counter() < deadline:
                    await conn.call(method, VALUE if method == ECHO else KEY_BYTES[rng.randrange(KEYS)])
                    done += 1

            began = time.perf_counter()
            await asyncio.gather(*(worker() for _ in range(depth)))
            emit(op=op, depth=depth, ops=done / (time.perf_counter() - began))


async def run_asyncio():
    loop = asyncio.get_running_loop()
    if TRANSPORT == "stdio":
        argv, env = sidecar_command()
        sidecar = await asyncio.create_subprocess_exec(*argv, env=env, stdin=asyncio.subprocess.PIPE,
                                                       stdout=asyncio.subprocess.PIPE, limit=1 << 20)
        conn = AsyncConn(sidecar.stdin.write)

        async def read():
            while data := await sidecar.stdout.read(1 << 16):
                conn.frames.feed(data)

        reading = asyncio.create_task(read())
        await measure_async(conn)
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
    feed.conn = AsyncConn(transport.write)
    await measure_async(feed.conn)
    transport.close()


class ThreadConn:
    """Blocking calls from many threads; one thread reads every answer."""

    def __init__(self, write, read):
        self.write_bytes = write
        self.writing = threading.Lock()
        self.lock = threading.Lock()
        self.pending = {}
        self.next = 0
        self.frames = Frames(self.answered)
        self.reader = threading.Thread(target=self.receive, args=(read,), daemon=True)
        self.reader.start()

    def receive(self, read):
        while data := read(1 << 16):
            self.frames.feed(data)

    def answered(self, stream, body):
        with self.lock:
            waiting = self.pending.pop(stream, None)
        if waiting is not None:
            waiting[1] = body
            waiting[0].set()

    def call(self, method, body):
        waiting = [threading.Event(), None]
        with self.lock:
            self.next = (self.next + 1) & 0xFFFFFFFF
            stream = self.next
            self.pending[stream] = waiting
        frame = HEADER.pack(len(body), REQUEST, END, method, stream) + body
        with self.writing:
            self.write_bytes(frame)
        waiting[0].wait()
        return waiting[1]


def measure_threads(conn):
    conn.call(ECHO, VALUE)  # the sidecar has filled its bucket
    for method, op in ((ECHO, "echo"), (GET, "get")):
        for depth in DEPTHS:
            counts = [0] * depth
            deadline = time.perf_counter() + SECONDS

            def worker(i):
                rng = random.Random()
                while time.perf_counter() < deadline:
                    conn.call(method, VALUE if method == ECHO else KEY_BYTES[rng.randrange(KEYS)])
                    counts[i] += 1

            began = time.perf_counter()
            workers = [threading.Thread(target=worker, args=(i,)) for i in range(depth)]
            for w in workers:
                w.start()
            for w in workers:
                w.join()
            emit(op=op, depth=depth, ops=sum(counts) / (time.perf_counter() - began))


def run_threads():
    if TRANSPORT == "stdio":
        argv, env = sidecar_command()
        sidecar = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0)

        def write(frame):
            sidecar.stdin.write(frame)

        measure_threads(ThreadConn(write, sidecar.stdout.read))
        sidecar.stdin.close()
        sidecar.wait()
        return
    if TRANSPORT == "pipe":
        emit(skipped="a handle Python opens is synchronous: a pending read holds every write")
        return
    if TRANSPORT == "unix":
        if not hasattr(socket, "AF_UNIX"):
            emit(skipped="no AF_UNIX in this Python")
            return
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(ADDRESS)
    else:
        host, port = ADDRESS.rsplit(":", 1)
        sock = socket.create_connection((host, int(port)))
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    measure_threads(ThreadConn(sock.sendall, sock.recv))
    sock.close()


if __name__ == "__main__":
    if sys.argv[1] == "asyncio":
        asyncio.run(run_asyncio())
    else:
        run_threads()
