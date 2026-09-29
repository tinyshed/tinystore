// The application's files in blobs/, as Go's blobs package keeps them:
// objects under keys that are paths, their bytes streamed both ways in DATA
// of 64 KiB at most, a whole read checked against the object's hash.

import { type Connection, download, type Link } from './connection.ts'
import { ClosedError, InvalidError, OutcomeUnknownError } from './errors.ts'
import { checkName, handleOn, type Page } from './handles.ts'
import { LostError, type Stream, watch } from './session.ts'
import { type Duration, dateOf, ms, type Time, unixMs } from './time.ts'
import type { Key } from './wire/codec.ts'
import {
	BlobsBucket,
	BlobsCall,
	BlobsObject,
	BlobsPage,
	BlobsTotal,
	methods,
} from './wire/messages.ts'

/** The most a DATA frame carries, so that no transfer holds the connection long. */
const chunk = 64 << 10

export interface BlobObject {
	/** its path under the folder that returned it */
	key: string
	size: number
	/** quoted, as an HTTP header carries it: equal bytes, one ETag */
	etag: string
	contentType: string | undefined
	modified: Date
	/** undefined for an object that does not expire */
	expires: Date | undefined
	meta: Record<string, string>
}

export interface WriteOptions {
	contentType?: string
	/** replaces all of an object's meta; a copy keeps its source's when absent */
	meta?: Record<string, string>
	ttl?: Duration
	expireAt?: Time
	/** writes only while the key holds a live object with one of these ETags, as If-Match spells them */
	ifMatch?: string
	/** writes only where no live object is */
	ifNoneMatch?: boolean
}

export interface PutOptions extends WriteOptions {
	/** the body's length, when it cannot say itself: one that disagrees leaves nothing */
	size?: number
	signal?: AbortSignal
}

/** What a put takes: bytes, text, a Blob or a file, or a stream of bytes. */
export type Body =
	| Uint8Array
	| ArrayBuffer
	| string
	| Blob
	| ReadableStream<Uint8Array>
	| AsyncIterable<Uint8Array>

/** An object being read: what it is, and its bytes as a stream to consume or cancel. */
export interface Download extends BlobObject {
	/** its bytes; a whole read whose bytes changed ends with CorruptError before its last */
	readonly body: ReadableStream<Uint8Array>
	bytes(): Promise<Uint8Array>
	text(): Promise<string>
	json(): Promise<unknown>
}

export class Blobs {
	readonly #link: Link

	constructor(link: Link) {
		this.#link = link
	}

	bucket(name: string, options?: { defaultTtl?: Duration; maxSize?: number }): BlobBucket {
		checkName(name, 'bucket')
		const open = BlobsBucket.encode({
			name,
			defaultTtl: options?.defaultTtl === undefined ? undefined : ms(options.defaultTtl),
			maxSize: options?.maxSize,
		})
		return new BlobBucket(this.#link, name, open, [])
	}
}

function ownerText(owner: Key): string | Uint8Array {
	if (typeof owner === 'number' || typeof owner === 'bigint') {
		if (typeof owner === 'number' && !Number.isSafeInteger(owner)) {
			throw new InvalidError(`the owner ${owner} is not an integer; an owner is text or an integer`)
		}
		return String(owner)
	}
	return owner
}

const lossy = new TextDecoder()

function objectOf(o: ReturnType<typeof BlobsObject.decode>): BlobObject {
	const meta: Record<string, string> = {}
	for (const [name, value] of Object.entries(o.meta ?? {})) {
		meta[name] = typeof value === 'string' ? value : lossy.decode(value)
	}
	return {
		key: o.key ?? '',
		size: o.size ?? 0,
		etag: o.etag ?? '',
		contentType: o.contentType,
		modified: new Date(o.modified ?? 0),
		expires: dateOf(o.expires),
		meta,
	}
}

function writeFields(options: WriteOptions | undefined) {
	return {
		contentType: options?.contentType,
		meta: options?.meta,
		ttl: options?.ttl === undefined ? undefined : ms(options.ttl),
		expireAt: options?.expireAt === undefined ? undefined : unixMs(options.expireAt),
		ifMatch: options?.ifMatch,
		ifNoneMatch: options?.ifNoneMatch === true ? true : undefined,
	}
}

type CallFields = Omit<Parameters<typeof BlobsCall.encode>[0], 'handle' | 'owners'>

/** A bucket's objects under one folder of owners, the root when there are none. */
export class BlobBucket {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #owners: (string | Uint8Array)[]

	constructor(link: Link, name: string, open: Uint8Array, owners: (string | Uint8Array)[]) {
		this.#link = link
		this.name = name
		this.#open = open
		this.#owners = owners
	}

	/** The folder below this one that the owners name, each a segment: `of('users', 7)`. */
	of(...owners: Key[]): BlobBucket {
		return new BlobBucket(this.#link, this.name, this.#open, [
			...this.#owners,
			...owners.map(ownerText),
		])
	}

	async #call(connection: Connection, fields: CallFields) {
		const handle = await handleOn(connection, methods['blobs.open'], this.#open)
		return BlobsCall.encode({
			handle,
			owners: this.#owners.length > 0 ? this.#owners : undefined,
			...fields,
		})
	}

	async #one(method: keyof typeof methods, fields: CallFields, idempotence: 'read' | 'write') {
		return this.#link.run(idempotence, async connection =>
			connection.session.call(methods[method], await this.#call(connection, fields)),
		)
	}

	/** The object at a key, undefined when it holds none. */
	async stat(key: string): Promise<BlobObject | undefined> {
		const o = BlobsObject.decode(await this.#one('blobs.stat', { key }, 'read'))
		return o.found === true ? objectOf(o) : undefined
	}

	async delete(key: string, options?: Pick<WriteOptions, 'ifMatch'>): Promise<void> {
		await this.#one('blobs.delete', { key, ifMatch: options?.ifMatch }, 'write')
	}

	/** Writes to a key an object naming the source's bytes, which are neither read nor written. */
	async copy(from: string, to: string, options?: WriteOptions): Promise<BlobObject> {
		return objectOf(
			BlobsObject.decode(
				await this.#one('blobs.copy', { key: from, to, ...writeFields(options) }, 'write'),
			),
		)
	}

	/** Copy and the source's removal in one write. */
	async move(from: string, to: string, options?: WriteOptions): Promise<BlobObject> {
		return objectOf(
			BlobsObject.decode(
				await this.#one('blobs.move', { key: from, to, ...writeFields(options) }, 'write'),
			),
		)
	}

	/** The objects under this folder and their bytes, counted from their rows. */
	async usage(): Promise<{ objects: number; bytes: number }> {
		const total = BlobsTotal.decode(await this.#one('blobs.usage', {}, 'read'))
		return { objects: total.objects ?? 0, bytes: total.bytes ?? 0 }
	}

	/** Removes this folder's objects and every folder's under it, at once however many. */
	async clear(): Promise<void> {
		await this.#one('blobs.clear', {}, 'write')
	}

	/** A page of the objects under this folder, its folders included, in the byte order of their paths. */
	async scan(options?: {
		prefix?: string
		after?: string
		limit?: number
	}): Promise<Page<BlobObject, string>> {
		const got = await this.#link.run('read', async connection =>
			download(
				connection,
				methods['blobs.scan'],
				await this.#call(connection, {
					prefix: options?.prefix,
					after: options?.after,
					limit: options?.limit,
				}),
			),
		)
		const page = BlobsPage.decode(got.trailer)
		return {
			items: got.items.map(item => objectOf(BlobsObject.decode(item))),
			next: page.more === true ? (page.after ?? '') : undefined,
		}
	}

	async *all(options?: { prefix?: string; limit?: number }): AsyncGenerator<BlobObject> {
		let after: string | undefined
		for (;;) {
			const page = await this.scan({ ...options, ...(after === undefined ? {} : { after }) })
			yield* page.items
			if (page.next === undefined) {
				return
			}
			after = page.next
		}
	}

	/**
	 * Writes an object, returning once it is durable: whatever the key held is
	 * replaced at the commit, whole, and an upload that does not commit leaves
	 * nothing. A Blob, a file among them, gives its size and its type.
	 */
	async put(key: string, body: Body, options?: PutOptions): Promise<BlobObject> {
		const source = sourceOf(body, options?.size)
		const fields = {
			...writeFields(options),
			contentType: options?.contentType ?? source.contentType,
		}
		return this.#link.run(
			'write',
			async connection => {
				const request = await this.#call(connection, { key, size: source.size, ...fields })
				return upload(connection, request, source, options?.signal)
			},
			options?.signal,
		)
	}

	/**
	 * Reads an object, undefined when the key holds none: what it is at once,
	 * and its bytes as its body is read. A range, from offset and length bytes
	 * long, is not checked against the object's hash.
	 */
	async get(
		key: string,
		options?: { offset?: number; length?: number; signal?: AbortSignal },
	): Promise<Download | undefined> {
		return this.#link.run(
			'read',
			async connection => {
				const request = await this.#call(connection, {
					key,
					offset: options?.offset,
					length: options?.length,
				})
				const stream = await connection.session.open(methods['blobs.get'], request, true)
				const unwatch = watch(options?.signal, stream)
				const head = await stream.next().catch(err => {
					unwatch()
					throw err
				})
				const o = BlobsObject.decode(head.body)
				if (head.end || o.found !== true) {
					unwatch()
					return undefined
				}
				return downloadOf(objectOf(o), stream, unwatch)
			},
			options?.signal,
		)
	}
}

/**
 * A body as chunks of 64 KiB at most, its size and type when it knows them,
 * and whether it reads again for a second try.
 */
interface Source {
	size: number | undefined
	contentType: string | undefined
	replayable: boolean
	chunks(): AsyncIterable<Uint8Array>
}

function sourceOf(body: Body, size: number | undefined): Source {
	if (typeof body === 'string') {
		body = new TextEncoder().encode(body)
	}
	if (body instanceof ArrayBuffer) {
		body = new Uint8Array(body)
	}
	if (body instanceof Uint8Array) {
		const bytes = body
		return {
			size: size ?? bytes.length,
			contentType: undefined,
			replayable: true,
			chunks: () => sliced(bytes),
		}
	}
	if (body instanceof Blob) {
		const blob = body
		const contentType = blob.type === '' ? undefined : blob.type
		return {
			size: size ?? blob.size,
			contentType,
			replayable: true,
			chunks: () => rechunked(blob.stream()),
		}
	}
	const stream = body
	return { size, contentType: undefined, replayable: false, chunks: () => rechunked(stream) }
}

async function* sliced(bytes: Uint8Array): AsyncGenerator<Uint8Array> {
	for (let at = 0; at < bytes.length; at += chunk) {
		yield bytes.subarray(at, at + chunk)
	}
}

async function* rechunked(
	stream: AsyncIterable<Uint8Array> | ReadableStream<Uint8Array>,
): AsyncGenerator<Uint8Array> {
	for await (const piece of stream as AsyncIterable<Uint8Array>) {
		yield* sliced(piece)
	}
}

/**
 * Sends a put's REQUEST, its bytes as DATA and the DATA that ends them, and
 * waits for the object the commit wrote. A connection lost before that last
 * DATA has left aborted the upload, which left nothing; lost after it, the
 * commit's outcome is unknown.
 */
async function upload(
	connection: Connection,
	request: Uint8Array,
	source: Source,
	signal?: AbortSignal,
) {
	const stream = await connection.session.open(methods['blobs.put'], request, false)
	const unwatch = watch(signal, stream)
	let ended = false
	try {
		let last: Uint8Array | undefined
		for await (const piece of source.chunks()) {
			if (piece.length === 0) {
				continue
			}
			if (last !== undefined) {
				await stream.send(last, false)
			}
			last = piece
		}
		ended = true
		await stream.send(last ?? new Uint8Array(0), true)
		const answer = await stream.next()
		return objectOf(BlobsObject.decode(answer.body))
	} catch (err) {
		if (err instanceof LostError) {
			if (ended) {
				throw new OutcomeUnknownError(
					`the connection was lost as the upload committed: ${err.message}`,
				)
			}
			if (!source.replayable) {
				throw new ClosedError(
					`the connection was lost during the upload, which left nothing: ${err.message}`,
				)
			}
			// the server aborted what it had of it, so the whole body goes again
			throw new LostError(err.message, false)
		}
		stream.cancel()
		throw err
	} finally {
		unwatch()
	}
}

function downloadOf(object: BlobObject, stream: Stream, unwatch: () => void): Download {
	const body = new ReadableStream<Uint8Array>({
		async pull(controller) {
			try {
				const event = await stream.next()
				stream.consumed(event.body.length)
				if (event.body.length > 0) {
					controller.enqueue(event.body)
				}
				if (event.end) {
					unwatch()
					controller.close()
				}
			} catch (err) {
				unwatch()
				controller.error(err)
			}
		},
		cancel() {
			unwatch()
			stream.cancel()
		},
	})
	const bytes = async () => new Uint8Array(await new Response(body).arrayBuffer())
	return {
		...object,
		body,
		bytes,
		text: async () => new TextDecoder().decode(await bytes()),
		json: async () => JSON.parse(new TextDecoder().decode(await bytes())),
	}
}
