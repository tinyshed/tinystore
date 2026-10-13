// A call made in place: written into the bytes that leave next, answered
// where its RESPONSE arrived, with the link's rules for a lost connection.
// The server here is a script: what it answers each REQUEST, on a
// connection of its own each time the link dials.

import { expect, spyOn, test } from 'bun:test'

import { Connection, Link } from '../src/connection.ts'
import { OutcomeUnknownError, ProtocolError } from '../src/errors.ts'
import { Bucket, bucketOpen, valuesOf } from '../src/kv.ts'
import { Session } from '../src/session.ts'
import { Flag, frame, headerSize, Kind, parseHeader } from '../src/wire/frame.ts'
import { Writer } from '../src/wire/msgpack.ts'
import { Handle, KvCall, KvEntry, KvWritten, methods, Welcome } from '../src/wire/protocol.ts'
import { caught } from './ways.ts'

/** What a scripted server does with a REQUEST: answers it with a body, or ends the connection. */
type Script = (method: number, body: Uint8Array) => Uint8Array | 'end'

interface Served {
	connection: Connection
	/** how many REQUESTs each write to the server held */
	writes: number[]
}

const welcome = Welcome.encode({
	protocol: 2,
	server: 'script/0',
	instance: new Uint8Array(16),
	capability: 'admin',
	maxBody: 1 << 20,
	inFlight: 64,
	connectionCredit: 1 << 20,
	streamCredit: 1 << 20,
	engines: ['kv'],
	now: 0,
})

/** A connection to a server that answers by the script, a turn after each write as a socket would. */
async function served(script: Script): Promise<Served> {
	const writes: number[] = []
	let session: Session | undefined
	const answer = (bytes: Uint8Array) => {
		let requests = 0
		for (let at = 0; at < bytes.length; ) {
			const h = parseHeader(bytes, at)
			const body = bytes.slice(at + headerSize, at + headerSize + h.length)
			at += headerSize + h.length
			if (h.kind === Kind.hello) {
				session?.receive(frame(Kind.welcome, 0, 0, 0, welcome))
			}
			if (h.kind !== Kind.request) {
				continue
			}
			requests++
			const answered = script(h.method, body)
			if (answered === 'end') {
				session?.end(new Error('the script ended the connection'))
				return
			}
			session?.receive(frame(Kind.response, Flag.end, 0, h.stream, answered))
		}
		if (requests > 0) {
			writes.push(requests)
		}
	}
	const transport = {
		write: (bytes: Uint8Array) => queueMicrotask(() => answer(bytes)),
		close: () => {},
	}
	session = new Session(bytes => transport.write(bytes), { client: 'test/0' })
	return { connection: await Connection.overPipe(session, transport), writes }
}

/** A script that opens a handle and answers every get with the key it asked for. */
const echo: Script = (method, body) => {
	if (method === methods['kv.bucket.open']) {
		return Handle.encode({ handle: 7 })
	}
	if (method === methods['kv.set']) {
		return KvWritten.encode({ written: true, version: new Uint8Array(8) })
	}
	const key = String(KvCall.decode(body).key)
	return KvEntry.encode({ found: true, value: new TextEncoder().encode(JSON.stringify(key)) })
}

function bucketOver(link: Link): Bucket<string> {
	return new Bucket<string>(
		link,
		'sessions',
		bucketOpen('sessions'),
		valuesOf(undefined) as never,
		[],
	)
}

test('calls made in one turn leave in one write, and each gets its own answer', async () => {
	const server = await served(echo)
	const bucket = bucketOver(new Link(async () => server.connection))
	expect(await bucket.get('first')).toBe('first')

	const before = server.writes.length
	const direct = spyOn(Session.prototype, 'request')
	const answers = await Promise.all(['a', 'b', 'c'].map(key => bucket.get(key)))
	const inPlace = direct.mock.results.filter(result => result.value === true).length
	direct.mockRestore()
	expect(answers).toEqual(['a', 'b', 'c'])
	expect(server.writes.slice(before)).toEqual([3])
	expect(inPlace).toBe(3)
})

test('a read whose connection ends goes on the next one, and a write says its outcome is unknown', async () => {
	let ending = false
	const dialled: Served[] = []
	const link = new Link(async () => {
		const server = await served((method, body) => {
			const first = dialled[0] === server
			return ending && first && method !== methods['kv.bucket.open'] ? 'end' : echo(method, body)
		})
		dialled.push(server)
		return server.connection
	})
	const bucket = bucketOver(link)
	expect(await bucket.get('warm')).toBe('warm')

	ending = true
	expect(await bucket.get('again')).toBe('again')
	expect(dialled.length).toBe(2)

	// the second connection lives; a write that loses one cannot be made again unasked
	ending = false
	expect(await bucket.get('warm')).toBe('warm')
	dialled.shift()
	ending = true
	expect(await caught(bucket.set('k', 'v'))).toBeInstanceOf(OutcomeUnknownError)
})

test('an answer that is not its message fails its call alone', async () => {
	let broken = false
	const server = await served((method, body) =>
		broken && method === methods['kv.get'] ? Uint8Array.of(0xc1) : echo(method, body),
	)
	const bucket = bucketOver(new Link(async () => server.connection))
	expect(await bucket.get('warm')).toBe('warm')

	broken = true
	expect(await caught(bucket.get('k'))).toBeInstanceOf(ProtocolError)
	broken = false
	expect(await bucket.get('after')).toBe('after')
})

test('stream IDs sharing a table slot resolve their own calls in any order', async () => {
	const writes: Uint8Array[] = []
	const session = new Session(bytes => writes.push(bytes), { client: 'test/0' })
	session.receive(
		frame(
			Kind.welcome,
			0,
			0,
			0,
			Welcome.encode({
				protocol: 2,
				maxBody: 1 << 20,
				inFlight: 2048,
				connectionCredit: 1 << 20,
			}),
		),
	)
	await session.welcomed
	const answers = Array.from(
		{ length: 1100 },
		(_, i) =>
			new Promise<number>((resolve, reject) => {
				expect(
					session.direct(
						methods['kv.get'],
						w => w.uint(i),
						r => r.uint(),
						resolve,
						reject,
					),
				).toBe(true)
			}),
	)
	await Bun.sleep(0)
	const ids: number[] = []
	for (const bytes of writes) {
		for (let at = 0; at < bytes.length; ) {
			const h = parseHeader(bytes, at)
			if (h.kind === Kind.request) ids.push(h.stream)
			at += headerSize + h.length
		}
	}
	for (let i = ids.length - 1; i >= 0; i--) {
		const answer = new Writer()
		answer.uint(i)
		session.receive(frame(Kind.response, Flag.end, 0, ids[i]!, answer.bytes()))
	}
	expect(await Promise.all(answers)).toEqual(Array.from({ length: 1100 }, (_, i) => i))
})

test('a prepared get keeps key validation and rebuilds its prefix after reconnecting', async () => {
	let ending = false
	let opened = 0
	const link = new Link(async () => {
		const handle = ++opened === 1 ? 300 : 70_000
		return (
			await served((method, body) => {
				if (method === methods['kv.bucket.open']) return Handle.encode({ handle })
				if (ending && opened === 1) return 'end'
				const decoded = KvCall.decode(body)
				expect(decoded.handle).toBe(handle)
				expect(decoded.under).toEqual(['owner', 'β'])
				return echo(method, body)
			})
		).connection
	})
	const bucket = bucketOver(link).under('owner', 'β')
	expect(await bucket.get('warm')).toBe('warm')
	expect(await bucket.get('λ')).toBe('λ')
	expect(await bucket.get(123n)).toBe('123')
	expect(await bucket.has('present')).toBe(true)
	expect(await caught(bucket.has(1.5))).toHaveProperty('code', 'invalid')
	expect(await caught(bucket.get(1.5))).toHaveProperty('code', 'invalid')
	expect(await bucket.get('after')).toBe('after')
	ending = true
	expect(await bucket.get('again')).toBe('again')
	expect(await bucket.get('new-prefix')).toBe('new-prefix')
})
