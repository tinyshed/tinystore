// The application's files, as plan/api/blobs.md has them: files by path in
// named sets, written whole and replaced whole, over protocol 2. A file's
// bytes go in pieces within the credit of the side that takes them, and a
// whole read ends CorruptError before its last piece when its bytes changed.

import type { Connection, Idempotence, Link } from './connection.ts'
import { ClosedError, InvalidError, OutcomeUnknownError } from './errors.ts'
import { checkName, handleOn, type Via, viaLink } from './handles.ts'
import { LostError, type Stream, watch } from './session.ts'
import { type Duration, dateOf, ms, unixMs } from './time.ts'
import {
	BlobsAt,
	BlobsDelete,
	BlobsExpire,
	BlobsFolder,
	BlobsFound,
	BlobsGot,
	BlobsHead,
	BlobsInfo,
	BlobsList,
	BlobsMove,
	BlobsOpen,
	BlobsPage,
	BlobsPiece,
	BlobsRead,
	BlobsUsage,
	BlobsWrite,
	BlobsWritten,
	methods,
} from './wire/protocol.ts'

/** A size in bytes: a number, or text in binary units, 64KiB or 2GiB; '2GB' does not compile. */
export type Size = number | `${number}${'KiB' | 'MiB' | 'GiB' | 'TiB'}`

export interface FilesOptions {
	/** the term of every file written without its own; not a ceiling */
	ttl?: Duration | undefined
	/** the largest file a write may leave: past it, a write is refused and leaves nothing */
	maxFileSize?: Size | undefined
}

/** What a file carries: what serving it needs. */
export interface FileInfo {
	/** its path within the folder it was asked from */
	path: string
	size: number
	/** the SHA-256 of its bytes in hex, quoted, as an HTTP ETag is */
	etag: string
	contentType: string
	lastModified: Date
	/** undefined for a file that does not expire */
	expires: Date | undefined
	meta: Record<string, string>
}

export interface FileOptions {
	contentType?: string | undefined
	meta?: Record<string, string> | undefined
	/** the file's own term, from this write */
	ttl?: Duration | undefined
	/** the ETag of the one file this write may replace: another is ConflictError */
	ifMatch?: string | undefined
	/** the body's length: one longer or shorter is refused and leaves nothing */
	size?: number | undefined
	signal?: AbortSignal | undefined
}

/** What a write takes: text, bytes, a Blob or a file, or a stream of bytes. */
export type Body =
	| string
	| Uint8Array
	| ArrayBuffer
	| Blob
	| ReadableStream<Uint8Array>
	| AsyncIterable<Uint8Array>

export interface FileListOptions {
	/** text within the folder: 'photos/1' finds photos/10.jpg too */
	prefix?: string | undefined
	/** a page's next */
	after?: string | undefined
	/** 1000 at most, and when absent */
	limit?: number | undefined
}

/** A page of a folder's files in the byte order of their paths, and where the next starts. */
export interface FilePage {
	files: FileInfo[]
	/** undefined on the last page */
	next: string | undefined
}

const encoder = new TextEncoder()
const decoder = new TextDecoder()
const nothing = new Uint8Array(0)

/** Files as `store.files` opens them. */
export function openFiles(link: Link, name: string, options?: FilesOptions): Files {
	checkName(name, 'files')
	const open = BlobsOpen.encode({
		name,
		ttl: options?.ttl === undefined ? undefined : ms(options.ttl),
		maxFileSize: options?.maxFileSize === undefined ? undefined : bytesOf(options.maxFileSize),
	})
	return new Files(link, name, open, [])
}

/**
 * A named set of files by path, or a folder of it, whose calls are the
 * folder's: a path in it is the folder's segments and then its own.
 */
export class Files {
	readonly name: string
	readonly #link: Link
	readonly #via: Via
	readonly #open: Uint8Array
	readonly #folder: string[]

	/** Files come from `store.files`. */
	constructor(link: Link, name: string, open: Uint8Array, folder: string[]) {
		this.name = name
		this.#link = link
		this.#via = viaLink(link)
		this.#open = open
		this.#folder = folder
	}

	/** The folder of these segments, one each: `folder('users', 42)` is users/42, never users/420. */
	folder(...segments: (string | number | bigint)[]): Files {
		return new Files(this.#link, this.name, this.#open, [
			...this.#folder,
			...segments.map(segmentOf),
		])
	}

	/**
	 * Writes the file whole and replaces whatever the path held, returning
	 * once it is on disk as the store's durability says. Nobody sees half of
	 * it, while it is written or after a crash.
	 */
	async put(path: string, body: Body, options?: FileOptions): Promise<FileInfo> {
		// a put that returned wrote: only a create writes nothing
		return (await this.#write(path, body, options, false)) as FileInfo
	}

	/** Writes the file only where the path holds none, and says whether it did. */
	async create(path: string, body: Body, options?: Omit<FileOptions, 'ifMatch'>): Promise<boolean> {
		return (await this.#write(path, body, options, true)) !== undefined
	}

	/**
	 * What the file carries, and its bytes: a file up to about a megabyte comes
	 * with them, and a larger one's are read when they are read, on its ETag,
	 * as S3's GET with If-Match reads them. Undefined when the path holds none.
	 */
	async get(path: string): Promise<StoredFile | undefined> {
		const answer = await this.#call(
			'blobs.get',
			handle => BlobsAt.encode({ handle, folder: this.#folder, path }),
			'read',
		)
		const got = BlobsGot.decode(answer)
		if (got.info === undefined) {
			return undefined
		}
		const info = infoOf(got.info)
		const bytes = got.bytes ?? nothing
		const whole = bytes.length === info.size ? bytes : undefined
		return new StoredFile(info, whole, (offset, length) =>
			this.#read(path, info.etag, offset, length),
		)
	}

	/** What the file carries and none of its bytes; undefined when the path holds none. */
	async head(path: string): Promise<FileInfo | undefined> {
		const answer = await this.#call(
			'blobs.head',
			handle => BlobsAt.encode({ handle, folder: this.#folder, path }),
			'read',
		)
		const head = BlobsHead.decode(answer)
		return head.info === undefined ? undefined : infoOf(head.info)
	}

	/** Deletes the file, or with ifMatch only that version of it; no file there is not an error. */
	async delete(path: string, options?: { ifMatch?: string | undefined }): Promise<void> {
		await this.#call(
			'blobs.delete',
			handle =>
				BlobsDelete.encode({ handle, folder: this.#folder, path, ifMatch: options?.ifMatch }),
			'write',
		)
	}

	/**
	 * A copy at `to` that shares the bytes, which are not copied. A file at
	 * `to` is ConflictError unless ifMatch names the version to replace.
	 */
	copy(from: string, to: string, options?: { ifMatch?: string | undefined }): Promise<FileInfo> {
		return this.#move('blobs.copy', from, to, options?.ifMatch)
	}

	/**
	 * The file moved to `to` in one write: a file there is ConflictError
	 * unless ifMatch names the version to replace, and none at `from` NotFoundError.
	 */
	rename(from: string, to: string, options?: { ifMatch?: string | undefined }): Promise<FileInfo> {
		return this.#move('blobs.rename', from, to, options?.ifMatch)
	}

	/** Sets when the file expires, a span from now or a moment; says whether there was a file. */
	async expire(path: string, after: Duration | Date): Promise<boolean> {
		const span = after instanceof Date ? Math.max(0, unixMs(after) - Date.now()) : ms(after)
		const answer = await this.#call(
			'blobs.expire',
			handle => BlobsExpire.encode({ handle, folder: this.#folder, path, after: span }),
			'write',
		)
		return BlobsFound.decode(answer).found === true
	}

	/** A page of the files in the folder and in the folders in it, in the byte order of their paths. */
	async list(options?: FileListOptions): Promise<FilePage> {
		const answer = await this.#call(
			'blobs.list',
			handle =>
				BlobsList.encode({
					handle,
					folder: this.#folder,
					prefix: options?.prefix ?? '',
					after: options?.after,
					limit: options?.limit,
				}),
			'read',
		)
		const page = BlobsPage.decode(answer)
		return { files: (page.files ?? []).map(infoOf), next: page.next }
	}

	/** Every file of the folder, a page at a time, holding nothing between pages. */
	async *all(options?: { prefix?: string | undefined }): AsyncGenerator<FileInfo> {
		let after: string | undefined
		for (;;) {
			const page = await this.list({ prefix: options?.prefix, after })
			yield* page.files
			if (page.next === undefined) {
				return
			}
			after = page.next
		}
	}

	/** The files in the folder and their bytes, as it is now: it counts, it does not limit. */
	async usage(): Promise<{ count: number; size: number }> {
		const answer = await this.#call(
			'blobs.usage',
			handle => BlobsFolder.encode({ handle, folder: this.#folder }),
			'read',
		)
		const usage = BlobsUsage.decode(answer)
		return { count: usage.count ?? 0, size: usage.size ?? 0 }
	}

	/** Removes the folder's every file and every folder in it, at once for every reader. */
	async clear(): Promise<void> {
		await this.#call(
			'blobs.clear',
			handle => BlobsFolder.encode({ handle, folder: this.#folder }),
			'write',
		)
	}

	/**
	 * Begins a file written a piece at a time, which appears at `commit` and
	 * never before: `await using upload = await files.upload(path)`, and
	 * leaving the block without commit aborts it.
	 */
	async upload(path: string, options?: FileOptions): Promise<Upload> {
		const connection = await this.#link.connection()
		const write = BlobsWrite.encode({
			...this.#fields(path, options),
			handle: await this.#handle(connection),
		})
		const stream = await connection.session.open(methods['blobs.upload'], write, false)
		return new Upload(stream, pieceOf(connection), options?.signal)
	}

	#handle(connection: Connection): Promise<number> {
		return handleOn(connection, methods['blobs.open'], this.#open)
	}

	#call(
		method: keyof typeof methods,
		body: (handle: number) => Uint8Array,
		idempotence: Idempotence,
	): Promise<Uint8Array> {
		return this.#via.call(methods['blobs.open'], this.#open, methods[method], body, idempotence)
	}

	/**
	 * A file's bytes as S3's GET reads them with a range and If-Match: on the
	 * ETag a get gave, ConflictError once the path holds another file or none;
	 * the whole file checked, a range not. Nothing is asked before the first read.
	 */
	#read(
		path: string,
		ifMatch: string,
		offset?: number,
		length?: number,
	): ReadableStream<Uint8Array> {
		let stream: Stream | undefined
		const open = () =>
			this.#link.run('read', async connection => {
				const read = BlobsRead.encode({
					handle: await this.#handle(connection),
					folder: this.#folder,
					path,
					ifMatch,
					offset,
					length,
				})
				const opened = await connection.session.open(methods['blobs.read'], read, true)
				return { opened, first: await opened.next() }
			})
		return new ReadableStream<Uint8Array>({
			async pull(controller) {
				try {
					if (stream === undefined) {
						// the RESPONSE's piece is no DATA, so it takes none of the credit
						const { opened, first } = await open()
						stream = opened
						enqueue(controller, first.body)
						if (first.end) {
							controller.close()
						}
						return
					}
					const event = await stream.next()
					if (event.end) {
						controller.close()
						return
					}
					stream.consumed(event.body.length)
					enqueue(controller, event.body)
				} catch (err) {
					controller.error(err)
				}
			},
			cancel() {
				stream?.cancel()
			},
		})
	}

	async #move(
		method: 'blobs.copy' | 'blobs.rename',
		from: string,
		to: string,
		ifMatch: string | undefined,
	): Promise<FileInfo> {
		const answer = await this.#call(
			method,
			handle => BlobsMove.encode({ handle, folder: this.#folder, from, to, ifMatch }),
			'write',
		)
		return infoOf(BlobsInfo.decode(answer))
	}

	#fields(path: string, options: FileOptions | undefined) {
		return {
			folder: this.#folder,
			path,
			contentType: options?.contentType,
			meta: options?.meta,
			ttl: options?.ttl === undefined ? undefined : ms(options.ttl),
			ifMatch: options?.ifMatch,
			size: options?.size,
		}
	}

	/**
	 * Writes a body: in one call when it fits a piece, else as an upload over
	 * a stream. What it wrote, nothing when a create found a file.
	 */
	async #write(
		path: string,
		body: Body,
		options: FileOptions | undefined,
		create: boolean,
	): Promise<FileInfo | undefined> {
		const source = sourceOf(body)
		const fields = {
			...this.#fields(path, options),
			contentType: options?.contentType ?? source.contentType,
			size: options?.size ?? source.size,
			create,
		}
		return this.#link.run(
			'write',
			async connection => {
				const handle = await this.#handle(connection)
				const piece = pieceOf(connection)
				const whole = await source.whole(piece)
				if (whole !== undefined) {
					const write = BlobsWrite.encode({ ...fields, handle, bytes: whole })
					return writtenOf(await connection.session.call(methods['blobs.put'], write))
				}
				const write = BlobsWrite.encode({ ...fields, handle })
				const stream = await connection.session.open(methods['blobs.upload'], write, false)
				return uploaded(stream, source, piece, options?.signal)
			},
			options?.signal,
		)
	}
}

/**
 * A file written a piece at a time, as `files.upload` begins it: bytes go in
 * with `write`, the file appears at `commit` and never before, and an upload
 * that ends otherwise leaves nothing.
 */
export class Upload implements AsyncDisposable {
	readonly #stream: Stream
	readonly #piece: Uint8Array
	readonly #unwatch: () => void
	#filled = 0
	/** the writes so far, one after another, so that two writes never interleave */
	#writing: Promise<void> = Promise.resolve()
	#done = false

	/** Uploads come from `files.upload`. */
	constructor(stream: Stream, piece: number, signal: AbortSignal | undefined) {
		this.#stream = stream
		this.#piece = new Uint8Array(piece)
		this.#unwatch = watch(signal, stream)
	}

	/** Takes bytes, text as UTF-8, waiting while the server catches up. */
	write(bytes: Uint8Array | string): Promise<void> {
		if (this.#done) {
			return Promise.reject(new InvalidError('the upload has ended'))
		}
		const written = this.#writing.then(() => this.#take(bytes))
		this.#writing = written.catch(() => {})
		return written
	}

	/**
	 * Publishes the file whole, replacing what the path held as `put` does,
	 * once it is on disk as the store's durability says.
	 */
	async commit(): Promise<FileInfo> {
		if (this.#done) {
			throw new InvalidError('the upload has ended')
		}
		this.#done = true
		let ending = false
		try {
			await this.#writing
			ending = true
			await this.#send(true)
			// the RESPONSE that began it, and then the file the commit wrote
			await this.#stream.next()
			return writtenOf((await this.#stream.next()).body) as FileInfo
		} catch (err) {
			throw lostAs(err, ending)
		} finally {
			this.#unwatch()
		}
	}

	/** Ends the upload, which leaves nothing; after `commit` it does nothing. */
	async abort(): Promise<void> {
		if (this.#done) {
			return
		}
		this.#done = true
		this.#stream.cancel()
		this.#unwatch()
	}

	async [Symbol.asyncDispose](): Promise<void> {
		await this.abort()
	}

	async #take(bytes: Uint8Array | string): Promise<void> {
		const chunk = typeof bytes === 'string' ? encoder.encode(bytes) : bytes
		try {
			for (let at = 0; at < chunk.length; ) {
				const taken = Math.min(this.#piece.length - this.#filled, chunk.length - at)
				this.#piece.set(chunk.subarray(at, at + taken), this.#filled)
				this.#filled += taken
				at += taken
				if (this.#filled === this.#piece.length) {
					await this.#send(false)
				}
			}
		} catch (err) {
			throw lostAs(err, false)
		}
	}

	/** Sends what the piece holds, the message a copy of it, so the piece fills again at once. */
	async #send(end: boolean): Promise<void> {
		const body = BlobsPiece.encode({ bytes: this.#piece.subarray(0, this.#filled) })
		this.#filled = 0
		await this.#stream.send(body, end)
	}
}

/**
 * Bytes of a file, read when they are read, as a Blob's are: `stream()`, or
 * all of them with `bytes()`, `text()`, `json()` or `arrayBuffer()`, which
 * hold them in memory. Each read is a read of its own.
 */
export class FileBytes {
	readonly size: number
	readonly #stream: () => ReadableStream<Uint8Array>

	/** File bytes come from `files.get` and `slice`. */
	constructor(size: number, stream: () => ReadableStream<Uint8Array>) {
		this.size = size
		this.#stream = stream
	}

	/** Its bytes as they come, the credit of each given back once it is taken. */
	stream(): ReadableStream<Uint8Array> {
		return this.#stream()
	}

	async bytes(): Promise<Uint8Array<ArrayBuffer>> {
		const whole = new Uint8Array(this.size)
		let at = 0
		for await (const piece of this.stream() as AsyncIterable<Uint8Array>) {
			whole.set(piece, at)
			at += piece.length
		}
		return at === whole.length ? whole : whole.slice(0, at)
	}

	async arrayBuffer(): Promise<ArrayBuffer> {
		return (await this.bytes()).buffer
	}

	async text(): Promise<string> {
		return decoder.decode(await this.bytes())
	}

	async json(): Promise<unknown> {
		return JSON.parse(await this.text())
	}
}

/**
 * A file as `get` found it: what it carries, and its bytes. A whole read is
 * checked against its SHA-256 and fails CorruptError before its last bytes
 * when they changed; `slice` reads a range, which is not checked.
 */
export class StoredFile extends FileBytes implements FileInfo {
	readonly path: string
	readonly etag: string
	readonly contentType: string
	readonly lastModified: Date
	readonly expires: Date | undefined
	readonly meta: Record<string, string>
	/** its bytes when they came with the get */
	readonly #whole: Uint8Array | undefined
	readonly #read: (offset?: number, length?: number) => ReadableStream<Uint8Array>

	/** Stored files come from `files.get`. */
	constructor(
		info: FileInfo,
		whole: Uint8Array | undefined,
		read: (offset?: number, length?: number) => ReadableStream<Uint8Array>,
	) {
		super(info.size, () => (whole === undefined ? read() : streamOf(whole)))
		this.path = info.path
		this.etag = info.etag
		this.contentType = info.contentType
		this.lastModified = info.lastModified
		this.expires = info.expires
		this.meta = info.meta
		this.#whole = whole
		this.#read = read
	}

	/**
	 * A range of its bytes, as Blob.slice takes one: an end left out is the
	 * file's, and a negative place counts back from it, so `slice(-500)` is the
	 * last 500 bytes.
	 */
	slice(start?: number, end?: number): FileBytes {
		const from = placeOf(start, this.size, 0)
		const to = Math.max(from, placeOf(end, this.size, this.size))
		const whole = this.#whole
		if (whole !== undefined) {
			const bytes = whole.subarray(from, to)
			return new FileBytes(bytes.length, () => streamOf(bytes))
		}
		return new FileBytes(to - from, () => this.#read(from, to - from))
	}
}

/** A place in bytes as Blob.slice reads one: negative from the end, and within the size. */
function placeOf(place: number | undefined, size: number, absent: number): number {
	if (place === undefined) {
		return absent
	}
	const at = Math.trunc(place)
	return at < 0 ? Math.max(size + at, 0) : Math.min(at, size)
}

/** Bytes in hand as a stream, a copy, so that a reader's changes stay its own. */
function streamOf(bytes: Uint8Array): ReadableStream<Uint8Array> {
	return new ReadableStream<Uint8Array>({
		start(controller) {
			if (bytes.length > 0) {
				controller.enqueue(bytes.slice())
			}
			controller.close()
		},
	})
}

/** Enqueues a piece's bytes, none when it carries none. */
function enqueue(controller: ReadableStreamDefaultController<Uint8Array>, body: Uint8Array): void {
	const bytes = BlobsPiece.decode(body).bytes ?? nothing
	if (bytes.length > 0) {
		controller.enqueue(bytes)
	}
}

/**
 * A body as its bytes come, its size and type when it says them, and whether
 * it reads again from its start for a second try.
 */
interface Source {
	size: number | undefined
	contentType: string | undefined
	replayable: boolean
	chunks(): Iterable<Uint8Array> | AsyncIterable<Uint8Array>
	/** its bytes when they are known to fit a piece, for a write in one call */
	whole(piece: number): Promise<Uint8Array | undefined>
}

function sourceOf(body: Body): Source {
	const bytes =
		typeof body === 'string'
			? encoder.encode(body)
			: body instanceof ArrayBuffer
				? new Uint8Array(body)
				: body
	if (bytes instanceof Uint8Array) {
		return {
			size: bytes.length,
			contentType: undefined,
			replayable: true,
			chunks: () => [bytes],
			whole: async piece => (bytes.length <= piece ? bytes : undefined),
		}
	}
	if (bytes instanceof Blob) {
		const blob = bytes
		return {
			size: blob.size,
			contentType: blob.type === '' ? undefined : blob.type,
			replayable: true,
			chunks: () => blob.stream() as AsyncIterable<Uint8Array>,
			whole: async piece =>
				blob.size <= piece ? new Uint8Array(await blob.arrayBuffer()) : undefined,
		}
	}
	const stream = bytes as AsyncIterable<Uint8Array>
	return {
		size: undefined,
		contentType: undefined,
		replayable: false,
		chunks: () => stream,
		whole: async () => undefined,
	}
}

/**
 * The bytes of chunks again in pieces of `size` bytes, the last shorter. A
 * chunk that holds a whole piece goes as it is, without a copy.
 */
async function* pieces(
	chunks: Iterable<Uint8Array> | AsyncIterable<Uint8Array>,
	size: number,
): AsyncGenerator<Uint8Array> {
	let piece = new Uint8Array(size)
	let filled = 0
	for await (const chunk of chunks) {
		let at = 0
		while (at < chunk.length) {
			if (filled === 0 && chunk.length - at >= size) {
				yield chunk.subarray(at, at + size)
				at += size
				continue
			}
			const taken = Math.min(size - filled, chunk.length - at)
			piece.set(chunk.subarray(at, at + taken), filled)
			filled += taken
			at += taken
			if (filled === size) {
				yield piece
				piece = new Uint8Array(size)
				filled = 0
			}
		}
	}
	if (filled > 0) {
		yield piece.subarray(0, filled)
	}
}

/**
 * Sends an upload's pieces as DATA within the stream's credit, the last
 * ending them, and waits for what the commit wrote. A connection lost before
 * that last DATA left aborted the upload, which left nothing, and a body that
 * reads again goes again on the next connection; lost after it, the
 * commit's outcome is unknown.
 */
async function uploaded(
	stream: Stream,
	source: Source,
	piece: number,
	signal: AbortSignal | undefined,
): Promise<FileInfo | undefined> {
	const unwatch = watch(signal, stream)
	let ending = false
	try {
		let last: Uint8Array | undefined
		for await (const bytes of pieces(source.chunks(), piece)) {
			if (last !== undefined) {
				await stream.send(BlobsPiece.encode({ bytes: last }), false)
			}
			last = bytes
		}
		ending = true
		await stream.send(BlobsPiece.encode({ bytes: last ?? nothing }), true)
		// the RESPONSE that began it, and then what the commit wrote
		await stream.next()
		return writtenOf((await stream.next()).body)
	} catch (err) {
		if (err instanceof LostError && !ending && source.replayable) {
			// the server aborted what it had of it, so the whole body goes again
			throw new LostError(err.message, false)
		}
		stream.cancel()
		throw lostAs(err, ending)
	} finally {
		unwatch()
	}
}

/** What a lost connection means for an upload: nothing left before its last DATA, unknown after. */
function lostAs(err: unknown, ending: boolean): unknown {
	if (!(err instanceof LostError)) {
		return err
	}
	if (ending) {
		return new OutcomeUnknownError(
			`the connection was lost as the upload committed: ${err.message}`,
		)
	}
	return new ClosedError(
		`the connection was lost during the upload, which left nothing: ${err.message}`,
	)
}

/**
 * The bytes a piece carries: half what the server takes on a stream before
 * it grants more, so that one piece goes while the last is written, less a
 * piece's own few bytes.
 */
function pieceOf(connection: Connection): number {
	const agreed = connection.session.agreed
	const most = Math.min(agreed?.maxBody ?? 1 << 20, agreed?.streamCredit ?? 1 << 20)
	return Math.max(1, Math.floor(most / 2) - 16)
}

function writtenOf(body: Uint8Array): FileInfo | undefined {
	const written = BlobsWritten.decode(body)
	return written.info === undefined ? undefined : infoOf(written.info)
}

function infoOf(info: ReturnType<typeof BlobsInfo.decode>): FileInfo {
	return {
		path: info.path ?? '',
		size: info.size ?? 0,
		etag: info.etag ?? '',
		contentType: info.contentType ?? '',
		lastModified: new Date(info.lastModified ?? 0),
		expires: dateOf(info.expires),
		meta: info.meta ?? {},
	}
}

/** A folder's segment as text, an integer its decimal spelling; the server refuses one that is not a segment. */
function segmentOf(segment: string | number | bigint): string {
	if (typeof segment === 'number' && !Number.isSafeInteger(segment)) {
		throw new InvalidError(`the folder segment ${segment}: a segment is text or an integer`)
	}
	return String(segment)
}

const binary: Record<string, number> = { KiB: 2 ** 10, MiB: 2 ** 20, GiB: 2 ** 30, TiB: 2 ** 40 }

/** A size's bytes. '2GB' is refused: it is 10⁹ bytes to some and 2³⁰ to others. */
function bytesOf(size: Size | string): number {
	if (typeof size === 'number') {
		if (!Number.isSafeInteger(size) || size < 0) {
			throw new InvalidError(`a size of ${size} bytes`)
		}
		return size
	}
	const parts = /^(\d+(?:\.\d+)?)(KiB|MiB|GiB|TiB)$/.exec(size.trim())
	if (parts === null) {
		throw new InvalidError(
			`the size ${JSON.stringify(size)}; write it in bytes, or as 64KiB, 10MiB, 2GiB or 1TiB`,
		)
	}
	return Math.floor(Number(parts[1]) * binary[parts[2]!]!)
}
