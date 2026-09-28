// A Bun client of the rpc round's sidecar, spike/rpc_spike_test.go: frames as
// docs/wire.md lays them out, every call a stream of its own, the frames asked
// for in one microtask turn written at once. No packages: the bodies are raw
// bytes, so no codec is measured. It prints a JSON line a case.

import net from "node:net";

const REQUEST = 3, END = 1;
const ECHO = 1, GET = 2;
const KEYS = 10_000;
const DEPTHS = [1, 64, 256];
const env = process.env;
const transport = env.TINYSTORE_RPC_TRANSPORT!;
const seconds = Number(env.TINYSTORE_RPC_SECONDS ?? "2");

class Conn {
  pending = new Map<number, (body: Uint8Array) => void>();
  next = 0;
  rest: Uint8Array | null = null;
  out: Uint8Array[] = [];
  outBytes = 0;
  scheduled = false;

  constructor(private write: (bytes: Uint8Array) => void) {}

  feed(chunk: Uint8Array) {
    let buf = chunk;
    if (this.rest) {
      buf = new Uint8Array(this.rest.length + chunk.length);
      buf.set(this.rest);
      buf.set(chunk, this.rest.length);
      this.rest = null;
    }
    const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    let at = 0;
    while (buf.length - at >= 12) {
      const stop = at + 12 + view.getUint32(at, true);
      if (stop > buf.length) break;
      const stream = view.getUint32(at + 8, true);
      const answer = this.pending.get(stream);
      if (answer) {
        this.pending.delete(stream);
        answer(buf.subarray(at + 12, stop));
      }
      at = stop;
    }
    if (at < buf.length) this.rest = buf.slice(at);
  }

  call(method: number, body: Uint8Array): Promise<Uint8Array> {
    const stream = (this.next = (this.next + 1) >>> 0);
    const frame = new Uint8Array(12 + body.length);
    const view = new DataView(frame.buffer);
    view.setUint32(0, body.length, true);
    frame[4] = REQUEST;
    frame[5] = END;
    view.setUint16(6, method, true);
    view.setUint32(8, stream, true);
    frame.set(body, 12);
    this.out.push(frame);
    this.outBytes += frame.length;
    if (!this.scheduled) {
      this.scheduled = true;
      queueMicrotask(() => this.flush());
    }
    return new Promise((resolve) => this.pending.set(stream, resolve));
  }

  flush() {
    this.scheduled = false;
    const all = new Uint8Array(this.outBytes);
    let at = 0;
    for (const frame of this.out) {
      all.set(frame, at);
      at += frame.length;
    }
    this.out = [];
    this.outBytes = 0;
    this.write(all);
  }
}

// a socket's write may take part of the bytes; the rest waits for its drain
function bunSocketWriter() {
  let socket: any = null;
  const queue: Uint8Array[] = [];
  const pump = () => {
    while (queue.length) {
      const bytes = queue[0];
      const wrote = socket.write(bytes);
      if (wrote === bytes.length) {
        queue.shift();
        continue;
      }
      queue[0] = bytes.subarray(Math.max(wrote, 0));
      return;
    }
  };
  return {
    attach(s: any) { socket = s; },
    write(bytes: Uint8Array) {
      queue.push(bytes);
      if (queue.length === 1) pump();
    },
    drain: pump,
  };
}

async function overBunSocket(): Promise<[Conn, () => void]> {
  const writer = bunSocketWriter();
  let conn!: Conn;
  const socket = { data: (_: any, data: Uint8Array) => conn.feed(data), drain: () => writer.drain() };
  const address = env.TINYSTORE_RPC_ADDRESS!;
  const connected = transport === "tcp"
    ? await Bun.connect({ hostname: "127.0.0.1", port: Number(address.split(":").pop()), socket })
    : await Bun.connect({ unix: address, socket });
  writer.attach(connected);
  conn = new Conn(writer.write);
  return [conn, () => connected.end()];
}

// a Windows named pipe through node:net, which Bun implements
async function overNodePipe(): Promise<[Conn, () => void]> {
  const socket = net.createConnection(env.TINYSTORE_RPC_ADDRESS!);
  await new Promise<void>((resolve, reject) => {
    socket.once("connect", () => resolve());
    socket.once("error", reject);
  });
  const conn = new Conn((bytes) => socket.write(bytes));
  socket.on("data", (data: Uint8Array) => conn.feed(data));
  return [conn, () => socket.end()];
}

async function overStdio(): Promise<[Conn, () => Promise<void>]> {
  const argv = JSON.parse(env.TINYSTORE_RPC_SIDECAR_COMMAND!);
  const sidecar = Bun.spawn(argv, {
    stdin: "pipe", stdout: "pipe", stderr: "inherit",
    env: { ...process.env, [env.TINYSTORE_RPC_SIDECAR_VARIABLE!]: env.TINYSTORE_RPC_SIDECAR_CONFIG! },
  });
  const conn = new Conn((bytes) => {
    sidecar.stdin.write(bytes);
    sidecar.stdin.flush();
  });
  const reading = (async () => {
    for await (const chunk of sidecar.stdout) conn.feed(chunk);
  })();
  return [conn, async () => {
    sidecar.stdin.end();
    await sidecar.exited;
    await reading;
  }];
}

async function measure(conn: Conn) {
  const value = new Uint8Array(100).fill(7);
  const keys = Array.from({ length: KEYS }, (_, i) => new TextEncoder().encode("k" + String(i).padStart(5, "0")));
  await conn.call(ECHO, value); // the sidecar has filled its bucket
  for (const [method, op] of [[ECHO, "echo"], [GET, "get"]] as const) {
    for (const depth of DEPTHS) {
      let done = 0;
      const deadline = performance.now() + seconds * 1000;
      const worker = async () => {
        while (performance.now() < deadline) {
          await conn.call(method, method === ECHO ? value : keys[(Math.random() * KEYS) | 0]);
          done++;
        }
      };
      const began = performance.now();
      await Promise.all(Array.from({ length: depth }, worker));
      console.log(JSON.stringify({ op, depth, ops: done / ((performance.now() - began) / 1000) }));
    }
  }
}

switch (transport) {
  case "tcp":
  case "unix": {
    const [conn, close] = await overBunSocket();
    await measure(conn);
    close();
    break;
  }
  case "pipe": {
    const [conn, close] = await overNodePipe();
    await measure(conn);
    close();
    break;
  }
  case "stdio": {
    const [conn, close] = await overStdio();
    await measure(conn);
    await close();
    break;
  }
}
process.exit(0);
