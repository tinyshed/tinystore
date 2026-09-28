// A Bun client of `tinystore serve`, server/spike/serve_spike_test.go: frames
// as docs/wire.md lays them out, their bytes the ones Go's wire wrote, given
// in TINYSTORE_BENCH_FRAMES, so that no codec is measured; every call a stream
// of its own, the frames asked for in one microtask written at once. It
// prints a JSON line a case.

import net from "node:net";

const WELCOME = 2, RESPONSE = 4, GOAWAY = 10;
const ERROR = 2;
const KEYS = 10_000;
const DEPTHS = [1, 64, 256];
const env = process.env;
const transport = env.TINYSTORE_BENCH_TRANSPORT!;
const address = env.TINYSTORE_BENCH_ADDRESS ?? "";
const seconds = Number(env.TINYSTORE_RPC_SECONDS ?? "2");
const given = JSON.parse(env.TINYSTORE_BENCH_FRAMES!);
const unhex = (text: string) => Uint8Array.from(text.match(/../g)!.map((b) => parseInt(b, 16)));
const hello = unhex(given.hello), open = unhex(given.open), handle = unhex(given.handle), get = unhex(given.get);
const digits: number = given.key;

type Answer = (flags: number, body: Uint8Array) => void;

class Conn {
  pending = new Map<number, Answer>();
  welcomed: ((kind: number) => void) | null = null;
  next = 1; // stream 1 is the open's
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
      const kind = buf[at + 4], flags = buf[at + 5], stream = view.getUint32(at + 8, true);
      if (kind === WELCOME || kind === GOAWAY) {
        this.welcomed?.(kind);
      } else if (kind === RESPONSE) { // CREDIT and PING need nothing of a case shorter than a silence
        const answer = this.pending.get(stream);
        if (answer) {
          this.pending.delete(stream);
          answer(flags, buf.subarray(at + 12, stop));
        }
      }
      at = stop;
    }
    if (at < buf.length) this.rest = buf.slice(at);
  }

  send(frame: Uint8Array) {
    this.out.push(frame);
    this.outBytes += frame.length;
    if (!this.scheduled) {
      this.scheduled = true;
      queueMicrotask(() => this.flush());
    }
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

  handshake(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.welcomed = (kind) => (kind === WELCOME ? resolve() : reject(new Error("the server said GOAWAY")));
      this.send(hello);
    });
  }

  call(frame: Uint8Array, stream: number): Promise<Uint8Array> {
    return new Promise((resolve, reject) => {
      this.pending.set(stream, (flags, body) =>
        (flags & ERROR) === 0 ? resolve(body) : reject(new Error("the server answered an error")));
      this.send(frame);
    });
  }

  stream(): number {
    this.next = (this.next + 1) >>> 0 || 2;
    return this.next;
  }

  get(k: number): Promise<Uint8Array> {
    const frame = get.slice();
    const stream = this.stream();
    new DataView(frame.buffer).setUint32(8, stream, true);
    const text = String(k).padStart(5, "0");
    for (let i = 0; i < 5; i++) frame[digits + i] = text.charCodeAt(i);
    return this.call(frame, stream);
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
  conn = new Conn(writer.write);
  const connected = transport === "tcp"
    ? await Bun.connect({ hostname: "127.0.0.1", port: Number(address.split(":").pop()), socket })
    : await Bun.connect({ unix: address, socket });
  writer.attach(connected);
  return [conn, () => connected.end()];
}

// a Windows named pipe through node:net, which Bun implements
async function overNodePipe(): Promise<[Conn, () => void]> {
  const socket = net.createConnection(address);
  await new Promise<void>((resolve, reject) => {
    socket.once("connect", () => resolve());
    socket.once("error", reject);
  });
  const conn = new Conn((bytes) => socket.write(bytes));
  socket.on("data", (data: Uint8Array) => conn.feed(data));
  return [conn, () => socket.end()];
}

async function overStdio(): Promise<[Conn, () => Promise<void>]> {
  const sidecar = Bun.spawn(JSON.parse(env.TINYSTORE_BENCH_COMMAND!), {
    stdin: "pipe", stdout: "pipe", stderr: "ignore",
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

function found(body: Uint8Array) {
  if (body.length < 3 || body[1] !== 1 || body[2] !== 0xc3) throw new Error("a key of the bucket was not found");
}

async function measure(conn: Conn) {
  await conn.handshake();
  const opened = await conn.call(open, 1);
  if (opened.join() !== handle.join()) throw new Error("kv.open answered another handle");
  for (const depth of DEPTHS) {
    let done = 0;
    const deadline = performance.now() + seconds * 1000;
    const worker = async () => {
      while (performance.now() < deadline) {
        found(await conn.get((Math.random() * KEYS) | 0));
        done++;
      }
    };
    const began = performance.now();
    await Promise.all(Array.from({ length: depth }, worker));
    console.log(JSON.stringify({ op: "get", depth, ops: done / ((performance.now() - began) / 1000) }));
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
