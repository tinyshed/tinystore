// The session without a server: a peer in the test reads the frames it
// writes and answers with frames of its own.

import { describe, expect, test } from 'bun:test'
import { createHmac } from 'node:crypto'
import { createServer } from 'node:net'

import {
	CancelledError,
	ClosedError,
	ConflictError,
	LimitError,
	ProtocolError,
	UnavailableError,
} from '../src/errors.ts'
import { LostError, Session, type SessionOptions } from '../src/session.ts'
import { connect } from '../src/store.ts'
import { credit, Flag, frame, headerSize, Kind, parseHeader } from '../src/wire/frame.ts'
import { Empty, Failure, GoAway, Hello, KvEntry, Welcome } from '../src/wire/messages.ts'

/**
 * What a promise rejected with. Bun's expect(...).rejects waits for a
 * promise without running the event loop's I/O, so a call answered by a
 * server never settles inside it.
 */
async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

interface Frame {
	kind: number
	flags: number
	method: number
	stream: number
	body: Uint8Array
}

/** The other end of a session: what it wrote, a write at a time, and a way to answer. */
class Peer {
	writes: Uint8Array[] = []
	session: Session

	constructor(options: Partial<SessionOptions> = {}) {
		this.session = new Session(bytes => this.writes.push(bytes), { client: 'test', ...options })
	}

	/** Every frame written since the last look, once the turn's writes have left. */
	async frames(): Promise<Frame[]> {
		await Bun.sleep(0)
		const frames: Frame[] = []
		for (const bytes of this.writes.splice(0)) {
			let at = 0
			while (at < bytes.length) {
				const h = parseHeader(bytes, at)
				frames.push({ ...h, body: bytes.slice(at + headerSize, at + headerSize + h.length) })
				at += headerSize + h.length
			}
		}
		return frames
	}

	welcome(fields: Partial<Parameters<typeof Welcome.encode>[0]> = {}): void {
		this.session.receive(
			frame(
				Kind.welcome,
				0,
				0,
				0,
				Welcome.encode({
					protocol: 1,
					server: 'test',
					capability: 'admin',
					maxBody: 1 << 20,
					inFlight: 4,
					connectionCredit: 1 << 20,
					streamCredit: 1 << 16,
					now: Date.now(),
					...fields,
				}),
			),
		)
	}

	answer(
		stream: number,
		body: Uint8Array,
		flags: number = Flag.end,
		kind: number = Kind.response,
	): void {
		this.session.receive(frame(kind, flags, 0, stream, body))
	}
}

const entry = (found: boolean) => KvEntry.encode({ found })

describe('the handshake', () => {
	test('a HELLO leaves first, stating the download window', async () => {
		const peer = new Peer()
		const [hello] = await peer.frames()
		expect(hello?.kind).toBe(Kind.hello)
		const decoded = Hello.decode(hello!.body)
		expect(decoded.protocol).toBe(1)
		expect(decoded.streamCredit).toBe(2 << 20)
	})

	test('a proof that checks lets the calls go; one that does not ends the connection', async () => {
		const secret = new Uint8Array(32).fill(7)
		const challenge = new Uint8Array(16).fill(9)
		const proof = createHmac('sha256', secret).update(challenge).digest()

		const good = new Peer({ secret, challenge })
		good.welcome({ proof })
		expect((await good.session.welcomed).capability).toBe('admin')

		const bad = new Peer({ secret, challenge })
		bad.welcome({ proof: new Uint8Array(32) })
		expect(await caught(bad.session.welcomed)).toBeInstanceOf(ProtocolError)
	})

	test('a GOAWAY where the WELCOME belongs is its code', async () => {
		const peer = new Peer()
		peer.session.receive(
			frame(
				Kind.goaway,
				0,
				0,
				0,
				GoAway.encode({ code: 'limit', message: 'too many connections' }),
			),
		)
		expect(await caught(peer.session.welcomed)).toBeInstanceOf(LimitError)
	})
})

describe('calls', () => {
	test('a call is a REQUEST and the RESPONSE that answers it, in any order', async () => {
		const peer = new Peer()
		peer.welcome()
		await peer.frames()
		const first = peer.session.call(0x0102, new Uint8Array([0x80]))
		const second = peer.session.call(0x0102, new Uint8Array([0x80]))
		const requests = await peer.frames()
		expect(requests.map(f => [f.kind, f.flags, f.method])).toEqual([
			[Kind.request, Flag.end, 0x0102],
			[Kind.request, Flag.end, 0x0102],
		])
		peer.answer(requests[1]!.stream, entry(true))
		peer.answer(requests[0]!.stream, entry(false))
		expect(KvEntry.decode(await second).found).toBe(true)
		expect(KvEntry.decode(await first).found).toBe(false)
	})

	test('the frames asked for in one turn leave in one write', async () => {
		const peer = new Peer()
		peer.welcome()
		await peer.frames()
		for (let i = 0; i < 3; i++) {
			peer.session.call(0x0102, new Uint8Array([0x80]))
		}
		await Bun.sleep(0)
		expect(peer.writes.length).toBe(1)
	})

	test('an error ends its call as its code', async () => {
		const peer = new Peer()
		peer.welcome()
		const call = peer.session.call(0x0104, new Uint8Array([0x80]))
		const [request] = (await peer.frames()).filter(f => f.kind === Kind.request)
		peer.answer(
			request!.stream,
			Failure.encode({ code: 'conflict', message: 'state changed', what: { bucket: 'sessions' } }),
			Flag.end | Flag.error,
		)
		const err = await call.catch(e => e)
		expect(err).toBeInstanceOf(ConflictError)
		expect(err.what).toEqual({ bucket: 'sessions' })
	})

	test('a request past the agreed body is refused before it leaves', async () => {
		const peer = new Peer()
		peer.welcome({ maxBody: 8 })
		expect(await caught(peer.session.call(0x0104, new Uint8Array(9)))).toBeInstanceOf(LimitError)
	})

	test('no more streams open than the WELCOME lets fly', async () => {
		const peer = new Peer()
		peer.welcome({ inFlight: 2 })
		await peer.frames()
		const calls = [0, 1, 2].map(() => peer.session.call(0x0102, new Uint8Array([0x80])))
		const flying = (await peer.frames()).filter(f => f.kind === Kind.request)
		expect(flying.length).toBe(2)
		peer.answer(flying[0]!.stream, entry(true))
		await calls[0]
		const third = (await peer.frames()).filter(f => f.kind === Kind.request)
		expect(third.length).toBe(1)
		expect(third[0]!.stream).not.toBe(flying[1]!.stream)
	})

	test("requests wait for the connection's credit, in the order they were asked for", async () => {
		const peer = new Peer()
		peer.welcome({ connectionCredit: 10 })
		await peer.frames()
		peer.session.call(0x0104, new Uint8Array(6))
		peer.session.call(0x0104, new Uint8Array(6))
		expect((await peer.frames()).length).toBe(1)
		peer.session.receive(credit(0, 6))
		const next = await peer.frames()
		expect(next.map(f => f.body.length)).toEqual([6])
	})
})

describe('streams', () => {
	test('a download grants its credit back once half the window is read', async () => {
		const peer = new Peer()
		peer.welcome()
		await peer.frames()
		const stream = await peer.session.open(0x010d, new Uint8Array([0x80]), true)
		await peer.frames()
		peer.answer(stream.id, Empty.encode({}), 0)
		const chunk = new Uint8Array(1 << 19)
		peer.answer(stream.id, chunk, 0, Kind.data)
		peer.answer(stream.id, chunk, 0, Kind.data)
		expect((await stream.next()).kind).toBe('response')
		stream.consumed((await stream.next()).body.length)
		expect((await peer.frames()).length).toBe(0)
		stream.consumed((await stream.next()).body.length)
		const [grant] = await peer.frames()
		expect(grant?.kind).toBe(Kind.credit)
		expect(new DataView(grant!.body.buffer).getUint32(0, true)).toBe(1 << 20)
	})

	test("an upload's DATA waits for the stream's credit", async () => {
		const peer = new Peer()
		peer.welcome({ streamCredit: 4 })
		await peer.frames()
		const stream = await peer.session.open(0x0309, new Uint8Array([0x80]), false)
		await stream.send(new Uint8Array(4), false)
		const sending = stream.send(new Uint8Array(4), true)
		const sent = await peer.frames()
		expect(sent.map(f => f.kind)).toEqual([Kind.request, Kind.data])
		peer.session.receive(credit(stream.id, 4))
		await sending
		expect((await peer.frames()).map(f => [f.kind, f.flags])).toEqual([[Kind.data, Flag.end]])
	})

	test('a call cancelled before its REQUEST leaves never leaves', async () => {
		const peer = new Peer()
		peer.welcome({ connectionCredit: 1 })
		await peer.frames()
		peer.session.call(0x0104, new Uint8Array([0x80])).catch(() => {})
		const controller = new AbortController()
		const cancelled = peer.session.call(0x0104, new Uint8Array(1), controller.signal)
		await Bun.sleep(0)
		controller.abort()
		expect(await caught(cancelled)).toBeInstanceOf(CancelledError)
		peer.session.receive(credit(0, 1))
		expect((await peer.frames()).filter(f => f.kind === Kind.request).length).toBe(1)
	})

	test('a call cancelled in flight sends CANCEL and keeps its number until the final frame', async () => {
		const peer = new Peer()
		peer.welcome()
		await peer.frames()
		const controller = new AbortController()
		const call = peer.session.call(0x0102, new Uint8Array([0x80]), controller.signal)
		const [request] = await peer.frames()
		controller.abort()
		expect(await caught(call)).toBeInstanceOf(CancelledError)
		const [cancel] = await peer.frames()
		expect([cancel?.kind, cancel?.stream]).toEqual([Kind.cancel, request!.stream])
		expect(peer.session.streams).toBe(1)
		peer.answer(request!.stream, entry(true))
		expect(peer.session.streams).toBe(0)
	})
})

describe('the end of a connection', () => {
	test('a GOAWAY refuses new streams and the REQUESTs that have not left', async () => {
		const peer = new Peer()
		peer.welcome({ connectionCredit: 1 })
		await peer.frames()
		const sent = peer.session.call(0x0104, new Uint8Array([0x80]))
		const unsent = peer.session.call(0x0104, new Uint8Array([0x80]))
		const [request] = (await peer.frames()).filter(f => f.kind === Kind.request)
		peer.session.receive(
			frame(Kind.goaway, 0, 0, 0, GoAway.encode({ code: 'unavailable', message: 'closing' })),
		)
		expect(await caught(unsent)).toBeInstanceOf(UnavailableError)
		await expect(peer.session.call(0x0102, new Uint8Array([0x80]))).rejects.toBeInstanceOf(
			UnavailableError,
		)
		peer.answer(request!.stream, entry(true))
		expect(KvEntry.decode(await sent).found).toBe(true)
	})

	test('a lost connection tells each stream whether its REQUEST had left', async () => {
		const peer = new Peer()
		peer.welcome({ connectionCredit: 1 })
		await peer.frames()
		const sent = peer.session.call(0x0104, new Uint8Array([0x80])).catch(e => e)
		const unsent = peer.session.call(0x0104, new Uint8Array([0x80])).catch(e => e)
		await peer.frames()
		peer.session.end(new Error('reset'))
		const [a, b] = await Promise.all([sent, unsent])
		expect(a).toBeInstanceOf(LostError)
		expect(a.sent).toBe(true)
		expect(b).toBeInstanceOf(LostError)
		expect(b.sent).toBe(false)
	})

	test('a PING is answered with its bytes', async () => {
		const peer = new Peer()
		peer.welcome()
		await peer.frames()
		const ping = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8])
		peer.session.receive(frame(Kind.ping, 0, 0, 0, ping))
		const [pong] = await peer.frames()
		expect(pong?.kind).toBe(Kind.pong)
		expect(pong?.body).toEqual(ping)
	})

	test('a frame that breaks the protocol ends the session', async () => {
		const peer = new Peer()
		peer.welcome()
		const call = peer.session.call(0x0102, new Uint8Array([0x80])).catch(e => e)
		await peer.frames()
		peer.session.receive(frame(Kind.request, Flag.end, 0x0102, 1, new Uint8Array([0x80])))
		expect(peer.session.ended).toBeInstanceOf(ProtocolError)
		expect(await call).toBeInstanceOf(LostError)
	})

	test('frames split anywhere read as whole', async () => {
		const peer = new Peer()
		const welcome = frame(
			Kind.welcome,
			0,
			0,
			0,
			Welcome.encode({
				protocol: 1,
				maxBody: 1 << 20,
				inFlight: 1,
				connectionCredit: 1 << 20,
				streamCredit: 1,
			}),
		)
		for (const b of welcome) {
			peer.session.receive(new Uint8Array([b]))
		}
		expect((await peer.session.welcomed).inFlight).toBe(1)
	})
})

// a server nobody listens on fails as the store's kind on every platform,
// not as the socket's error, which differs between them
test('an address nobody listens on is closed', async () => {
	const probe = createServer()
	await new Promise<void>(listening => probe.listen(0, '127.0.0.1', listening))
	const { port } = probe.address() as { port: number }
	await new Promise(closed => probe.close(closed))
	const err = await caught(connect(`tcp://127.0.0.1:${port}`, { token: 'unused' }))
	expect(err).toBeInstanceOf(ClosedError)
	expect((err as Error).message).toContain(`cannot reach tcp://127.0.0.1:${port}`)
})
