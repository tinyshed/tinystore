// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.

import {
	bin,
	bool,
	float,
	int,
	int64,
	key,
	kvValue,
	list,
	message,
	names,
	type Raw,
	type Read,
	readNames,
	type SqlValue,
	sqlValue,
	str,
	uint,
	writeNames,
} from './codec.ts'

export const methods = {
	'blobs.open': 0x0401,
	'blobs.put': 0x0402,
	'blobs.upload': 0x0403,
	'blobs.get': 0x0404,
	'blobs.head': 0x0405,
	'blobs.delete': 0x0406,
	'blobs.copy': 0x0407,
	'blobs.rename': 0x0408,
	'blobs.expire': 0x0409,
	'blobs.list': 0x040a,
	'blobs.usage': 0x040b,
	'blobs.clear': 0x040c,
	'blobs.read': 0x040d,
	'jobs.queue.open': 0x0201,
	'jobs.schedule.open': 0x0202,
	'jobs.add': 0x0203,
	'jobs.set': 0x0204,
	'jobs.update': 0x0205,
	'jobs.cancel': 0x0206,
	'jobs.get': 0x0207,
	'jobs.list': 0x0208,
	'jobs.work': 0x0209,
	'jobs.step': 0x020a,
	'jobs.keep': 0x020b,
	'jobs.watch': 0x020c,
	'jobs.tx': 0x020d,
	'kv.bucket.open': 0x0101,
	'kv.get': 0x0102,
	'kv.has': 0x0103,
	'kv.set': 0x0104,
	'kv.create': 0x0105,
	'kv.take': 0x0106,
	'kv.delete': 0x0107,
	'kv.expire': 0x0108,
	'kv.clear': 0x0109,
	'kv.list': 0x010a,
	'kv.counters.open': 0x0110,
	'kv.counters.add': 0x0111,
	'kv.counters.get': 0x0112,
	'kv.counters.delete': 0x0113,
	'kv.counters.clear': 0x0114,
	'kv.rateLimit.open': 0x0120,
	'kv.quota.open': 0x0121,
	'kv.allow': 0x0122,
	'kv.peek': 0x0123,
	'kv.reset': 0x0124,
	'kv.refund': 0x0125,
	'kv.once.open': 0x0130,
	'kv.once.run': 0x0131,
	'kv.once.get': 0x0132,
	'kv.once.delete': 0x0133,
	'kv.tx': 0x0140,
	/**
	 * Stops the server as its closing does: the streams running finish, every
	 * connection is told GOAWAY, and a sidecar gives back SERVE and its directory.
	 */
	'server.stop': 0x0001,
	'server.clock': 0x0002,
	'sql.open': 0x0301,
	'sql.query': 0x0302,
	'sql.exec': 0x0303,
	'sql.batch': 0x0304,
	'sql.tx': 0x0305,
	'sql.columns': 0x0306,
} as const

export type Method = keyof typeof methods

/**
 * Opens a set of files by name, made the first time.
 */
export const BlobsOpen = message(
	'blobs.Open',
	{
		/** [a-z0-9][a-z0-9_-]{0,63} */
		name: [1, str],
		/** the term of every file written without its own */
		ttl: [2, uint],
		/** the largest file a write may leave */
		maxFileSize: [3, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.ttl !== undefined) {
				w.field(2)
				w.uint(v.ttl)
				n++
			}
			if (v.maxFileSize !== undefined) {
				w.field(3)
				w.uint(v.maxFileSize)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $ttl: number | undefined
			let $maxFileSize: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$ttl = r.uint()
						break
					case 3:
						$maxFileSize = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, ttl: $ttl, maxFileSize: $maxFileSize }
		},
	},
)

/**
 * Where a call stands: the files, a folder's segments, a path within it.
 */
export const BlobsAt = message(
	'blobs.At',
	{
		handle: [1, uint],
		folder: [2, list(str)],
		path: [3, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.folder !== undefined) {
				w.field(2)
				w.array(v.folder.length)
				for (const item0 of v.folder) {
					w.str(item0)
				}
				n++
			}
			if (v.path !== undefined) {
				w.field(3)
				w.str(v.path)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $folder: string[] | undefined
			let $path: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$folder = items0
						break
					}
					case 3:
						$path = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, folder: $folder, path: $path }
		},
	},
)

/**
 * What a file carries: what serving it needs.
 */
export const BlobsInfo = message(
	'blobs.Info',
	{
		/** within the folder the call stood in */
		path: [1, str],
		size: [2, uint],
		/** the SHA-256 of its bytes in hex, quoted */
		etag: [3, str],
		contentType: [4, str],
		lastModified: [5, int],
		expires: [6, int],
		meta: [7, names(str)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.path !== undefined) {
				w.field(1)
				w.str(v.path)
				n++
			}
			if (v.size !== undefined) {
				w.field(2)
				w.uint(v.size)
				n++
			}
			if (v.etag !== undefined) {
				w.field(3)
				w.str(v.etag)
				n++
			}
			if (v.contentType !== undefined) {
				w.field(4)
				w.str(v.contentType)
				n++
			}
			if (v.lastModified !== undefined) {
				w.field(5)
				w.int(v.lastModified)
				n++
			}
			if (v.expires !== undefined) {
				w.field(6)
				w.int(v.expires)
				n++
			}
			if (v.meta !== undefined) {
				w.field(7)
				writeNames(w, v.meta, str.write)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $path: string | undefined
			let $size: number | undefined
			let $etag: string | undefined
			let $contentType: string | undefined
			let $lastModified: number | undefined
			let $expires: number | undefined
			let $meta: Record<string, string> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$path = r.str()
						break
					case 2:
						$size = r.uint()
						break
					case 3:
						$etag = r.str()
						break
					case 4:
						$contentType = r.str()
						break
					case 5:
						$lastModified = r.int()
						break
					case 6:
						$expires = r.int()
						break
					case 7:
						$meta = readNames(r, str.read)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				path: $path,
				size: $size,
				etag: $etag,
				contentType: $contentType,
				lastModified: $lastModified,
				expires: $expires,
				meta: $meta,
			}
		},
	},
)

/**
 * A write of a whole file: a put, or with create a write only where no file
 * is. Its bytes are in the message, or, for an upload, the DATA that follow.
 */
export const BlobsWrite = message(
	'blobs.Write',
	{
		handle: [1, uint],
		folder: [2, list(str)],
		path: [3, str],
		contentType: [4, str],
		meta: [5, names(str)],
		/** the file's own term, from its write */
		ttl: [6, uint],
		/** the ETag of the one file it may replace */
		ifMatch: [7, str],
		/** the body's length: one longer or shorter is refused */
		size: [8, uint],
		/** writes only where no file is */
		create: [9, bool],
		/** a put's whole body; nothing for an upload */
		bytes: [10, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.folder !== undefined) {
				w.field(2)
				w.array(v.folder.length)
				for (const item0 of v.folder) {
					w.str(item0)
				}
				n++
			}
			if (v.path !== undefined) {
				w.field(3)
				w.str(v.path)
				n++
			}
			if (v.contentType !== undefined) {
				w.field(4)
				w.str(v.contentType)
				n++
			}
			if (v.meta !== undefined) {
				w.field(5)
				writeNames(w, v.meta, str.write)
				n++
			}
			if (v.ttl !== undefined) {
				w.field(6)
				w.uint(v.ttl)
				n++
			}
			if (v.ifMatch !== undefined) {
				w.field(7)
				w.str(v.ifMatch)
				n++
			}
			if (v.size !== undefined) {
				w.field(8)
				w.uint(v.size)
				n++
			}
			if (v.create !== undefined) {
				w.field(9)
				w.bool(v.create)
				n++
			}
			if (v.bytes !== undefined) {
				w.field(10)
				w.bin(v.bytes)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $folder: string[] | undefined
			let $path: string | undefined
			let $contentType: string | undefined
			let $meta: Record<string, string> | undefined
			let $ttl: number | undefined
			let $ifMatch: string | undefined
			let $size: number | undefined
			let $create: boolean | undefined
			let $bytes: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$folder = items0
						break
					}
					case 3:
						$path = r.str()
						break
					case 4:
						$contentType = r.str()
						break
					case 5:
						$meta = readNames(r, str.read)
						break
					case 6:
						$ttl = r.uint()
						break
					case 7:
						$ifMatch = r.str()
						break
					case 8:
						$size = r.uint()
						break
					case 9:
						$create = r.bool()
						break
					case 10:
						$bytes = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				handle: $handle,
				folder: $folder,
				path: $path,
				contentType: $contentType,
				meta: $meta,
				ttl: $ttl,
				ifMatch: $ifMatch,
				size: $size,
				create: $create,
				bytes: $bytes,
			}
		},
	},
)

/**
 * What a write wrote: none when a create found a file there.
 */
export const BlobsWritten = message(
	'blobs.Written',
	{
		info: [1, BlobsInfo],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.info !== undefined) {
				w.field(1)
				BlobsInfo.write(w, v.info)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $info: Read<typeof BlobsInfo.fields> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$info = BlobsInfo.read(r)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { info: $info }
		},
	},
)

/**
 * A piece of a file's bytes: an upload's DATA from the client, a read's from
 * the server.
 */
export const BlobsPiece = message(
	'blobs.Piece',
	{
		bytes: [1, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.bytes !== undefined) {
				w.field(1)
				w.bin(v.bytes)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $bytes: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$bytes = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { bytes: $bytes }
		},
	},
)

/**
 * A get's answer: what the file carries, none when there is no file, and all
 * its bytes when they fit one message; a larger file's are left to blobs.read,
 * so that a read of a range sends no byte before it.
 */
export const BlobsGot = message(
	'blobs.Got',
	{
		info: [1, BlobsInfo],
		/** the whole file, or nothing when it is larger */
		bytes: [2, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.info !== undefined) {
				w.field(1)
				BlobsInfo.write(w, v.info)
				n++
			}
			if (v.bytes !== undefined) {
				w.field(2)
				w.bin(v.bytes)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $info: Read<typeof BlobsInfo.fields> | undefined
			let $bytes: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$info = BlobsInfo.read(r)
						break
					case 2:
						$bytes = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { info: $info, bytes: $bytes }
		},
	},
)

/**
 * A read of a file's bytes, as S3's GET with a range and If-Match: the whole
 * file, checked, its last piece sent once its bytes matched their SHA-256 and a
 * changed byte ending the stream corrupt instead; or a range, not checked.
 */
export const BlobsRead = message(
	'blobs.Read',
	{
		handle: [1, uint],
		folder: [2, list(str)],
		path: [3, str],
		/** the ETag a get gave: another file there, or none, is a conflict */
		ifMatch: [4, str],
		/** from the first byte when absent */
		offset: [5, uint],
		/** to the last byte when absent */
		length: [6, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.folder !== undefined) {
				w.field(2)
				w.array(v.folder.length)
				for (const item0 of v.folder) {
					w.str(item0)
				}
				n++
			}
			if (v.path !== undefined) {
				w.field(3)
				w.str(v.path)
				n++
			}
			if (v.ifMatch !== undefined) {
				w.field(4)
				w.str(v.ifMatch)
				n++
			}
			if (v.offset !== undefined) {
				w.field(5)
				w.uint(v.offset)
				n++
			}
			if (v.length !== undefined) {
				w.field(6)
				w.uint(v.length)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $folder: string[] | undefined
			let $path: string | undefined
			let $ifMatch: string | undefined
			let $offset: number | undefined
			let $length: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$folder = items0
						break
					}
					case 3:
						$path = r.str()
						break
					case 4:
						$ifMatch = r.str()
						break
					case 5:
						$offset = r.uint()
						break
					case 6:
						$length = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				handle: $handle,
				folder: $folder,
				path: $path,
				ifMatch: $ifMatch,
				offset: $offset,
				length: $length,
			}
		},
	},
)

export const BlobsHead = message(
	'blobs.Head',
	{
		info: [1, BlobsInfo],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.info !== undefined) {
				w.field(1)
				BlobsInfo.write(w, v.info)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $info: Read<typeof BlobsInfo.fields> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$info = BlobsInfo.read(r)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { info: $info }
		},
	},
)

export const BlobsDelete = message(
	'blobs.Delete',
	{
		handle: [1, uint],
		folder: [2, list(str)],
		path: [3, str],
		ifMatch: [4, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.folder !== undefined) {
				w.field(2)
				w.array(v.folder.length)
				for (const item0 of v.folder) {
					w.str(item0)
				}
				n++
			}
			if (v.path !== undefined) {
				w.field(3)
				w.str(v.path)
				n++
			}
			if (v.ifMatch !== undefined) {
				w.field(4)
				w.str(v.ifMatch)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $folder: string[] | undefined
			let $path: string | undefined
			let $ifMatch: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$folder = items0
						break
					}
					case 3:
						$path = r.str()
						break
					case 4:
						$ifMatch = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, folder: $folder, path: $path, ifMatch: $ifMatch }
		},
	},
)

/**
 * A copy or a rename: from a path to another of the same folder. A file at
 * to is a conflict, unless ifMatch names the version to replace.
 */
export const BlobsMove = message(
	'blobs.Move',
	{
		handle: [1, uint],
		folder: [2, list(str)],
		from: [3, str],
		to: [4, str],
		ifMatch: [5, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.folder !== undefined) {
				w.field(2)
				w.array(v.folder.length)
				for (const item0 of v.folder) {
					w.str(item0)
				}
				n++
			}
			if (v.from !== undefined) {
				w.field(3)
				w.str(v.from)
				n++
			}
			if (v.to !== undefined) {
				w.field(4)
				w.str(v.to)
				n++
			}
			if (v.ifMatch !== undefined) {
				w.field(5)
				w.str(v.ifMatch)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $folder: string[] | undefined
			let $from: string | undefined
			let $to: string | undefined
			let $ifMatch: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$folder = items0
						break
					}
					case 3:
						$from = r.str()
						break
					case 4:
						$to = r.str()
						break
					case 5:
						$ifMatch = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, folder: $folder, from: $from, to: $to, ifMatch: $ifMatch }
		},
	},
)

export const BlobsExpire = message(
	'blobs.Expire',
	{
		handle: [1, uint],
		folder: [2, list(str)],
		path: [3, str],
		after: [4, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.folder !== undefined) {
				w.field(2)
				w.array(v.folder.length)
				for (const item0 of v.folder) {
					w.str(item0)
				}
				n++
			}
			if (v.path !== undefined) {
				w.field(3)
				w.str(v.path)
				n++
			}
			if (v.after !== undefined) {
				w.field(4)
				w.uint(v.after)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $folder: string[] | undefined
			let $path: string | undefined
			let $after: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$folder = items0
						break
					}
					case 3:
						$path = r.str()
						break
					case 4:
						$after = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, folder: $folder, path: $path, after: $after }
		},
	},
)

export const BlobsFound = message(
	'blobs.Found',
	{
		found: [1, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.found !== undefined) {
				w.field(1)
				w.bool(v.found)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $found: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$found = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { found: $found }
		},
	},
)

/**
 * A page of a folder's files, in the byte order of their paths.
 */
export const BlobsList = message(
	'blobs.List',
	{
		handle: [1, uint],
		folder: [2, list(str)],
		/** text within the folder */
		prefix: [3, str],
		/** a page's next */
		after: [4, str],
		/** 1000 at most, and when absent */
		limit: [5, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.folder !== undefined) {
				w.field(2)
				w.array(v.folder.length)
				for (const item0 of v.folder) {
					w.str(item0)
				}
				n++
			}
			if (v.prefix !== undefined) {
				w.field(3)
				w.str(v.prefix)
				n++
			}
			if (v.after !== undefined) {
				w.field(4)
				w.str(v.after)
				n++
			}
			if (v.limit !== undefined) {
				w.field(5)
				w.uint(v.limit)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $folder: string[] | undefined
			let $prefix: string | undefined
			let $after: string | undefined
			let $limit: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$folder = items0
						break
					}
					case 3:
						$prefix = r.str()
						break
					case 4:
						$after = r.str()
						break
					case 5:
						$limit = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, folder: $folder, prefix: $prefix, after: $after, limit: $limit }
		},
	},
)

export const BlobsPage = message(
	'blobs.Page',
	{
		files: [1, list(BlobsInfo)],
		next: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.files !== undefined) {
				w.field(1)
				w.array(v.files.length)
				for (const item0 of v.files) {
					BlobsInfo.write(w, item0)
				}
				n++
			}
			if (v.next !== undefined) {
				w.field(2)
				w.str(v.next)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $files: Read<typeof BlobsInfo.fields>[] | undefined
			let $next: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof BlobsInfo.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(BlobsInfo.read(r))
						}
						r.leave()
						$files = items0
						break
					}
					case 2:
						$next = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { files: $files, next: $next }
		},
	},
)

export const BlobsFolder = message(
	'blobs.Folder',
	{
		handle: [1, uint],
		folder: [2, list(str)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.folder !== undefined) {
				w.field(2)
				w.array(v.folder.length)
				for (const item0 of v.folder) {
					w.str(item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $folder: string[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$folder = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, folder: $folder }
		},
	},
)

export const BlobsUsage = message(
	'blobs.Usage',
	{
		count: [1, uint],
		size: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.count !== undefined) {
				w.field(1)
				w.uint(v.count)
				n++
			}
			if (v.size !== undefined) {
				w.field(2)
				w.uint(v.size)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $count: number | undefined
			let $size: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$count = r.uint()
						break
					case 2:
						$size = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { count: $count, size: $size }
		},
	},
)

/**
 * What a client says first.
 */
export const Hello = message(
	'Hello',
	{
		/** the newest the client speaks */
		protocol: [1, uint],
		/** a name and version, for logs */
		client: [2, str],
		/** required on TCP */
		token: [3, str],
		/** the largest body the client takes; the server's when absent */
		maxBody: [4, uint],
		/** the DATA the server may send a stream before the client grants more; 2 MiB when absent */
		streamCredit: [5, uint],
		/** 16 random bytes, when the client found the server through SERVE */
		challenge: [6, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.protocol !== undefined) {
				w.field(1)
				w.uint(v.protocol)
				n++
			}
			if (v.client !== undefined) {
				w.field(2)
				w.str(v.client)
				n++
			}
			if (v.token !== undefined) {
				w.field(3)
				w.str(v.token)
				n++
			}
			if (v.maxBody !== undefined) {
				w.field(4)
				w.uint(v.maxBody)
				n++
			}
			if (v.streamCredit !== undefined) {
				w.field(5)
				w.uint(v.streamCredit)
				n++
			}
			if (v.challenge !== undefined) {
				w.field(6)
				w.bin(v.challenge)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $protocol: number | undefined
			let $client: string | undefined
			let $token: string | undefined
			let $maxBody: number | undefined
			let $streamCredit: number | undefined
			let $challenge: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$protocol = r.uint()
						break
					case 2:
						$client = r.str()
						break
					case 3:
						$token = r.str()
						break
					case 4:
						$maxBody = r.uint()
						break
					case 5:
						$streamCredit = r.uint()
						break
					case 6:
						$challenge = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				protocol: $protocol,
				client: $client,
				token: $token,
				maxBody: $maxBody,
				streamCredit: $streamCredit,
				challenge: $challenge,
			}
		},
	},
)

/**
 * What a server answers HELLO with: what the connection agrees.
 */
export const Welcome = message(
	'Welcome',
	{
		/** the one this connection speaks */
		protocol: [1, uint],
		/** its version */
		server: [2, str],
		/** 16 random bytes a start */
		instance: [3, bin],
		/** admin, data or read */
		capability: [4, str],
		/** the largest body either side sends */
		maxBody: [5, uint],
		/** the streams a client may have open at once */
		inFlight: [6, uint],
		/** the REQUEST and DATA bytes a client may send before credit comes back */
		connectionCredit: [7, uint],
		/** the DATA bytes a client may send a stream before credit comes back */
		streamCredit: [8, uint],
		/** what this server serves */
		engines: [9, list(str)],
		/** the store's clock */
		now: [10, int],
		/** the HMAC-SHA256 of HELLO's challenge, keyed with SERVE's secret */
		proof: [11, bin],
		/** full or os, when the store was opened with one: how far its files' commits go */
		durability: [12, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.protocol !== undefined) {
				w.field(1)
				w.uint(v.protocol)
				n++
			}
			if (v.server !== undefined) {
				w.field(2)
				w.str(v.server)
				n++
			}
			if (v.instance !== undefined) {
				w.field(3)
				w.bin(v.instance)
				n++
			}
			if (v.capability !== undefined) {
				w.field(4)
				w.str(v.capability)
				n++
			}
			if (v.maxBody !== undefined) {
				w.field(5)
				w.uint(v.maxBody)
				n++
			}
			if (v.inFlight !== undefined) {
				w.field(6)
				w.uint(v.inFlight)
				n++
			}
			if (v.connectionCredit !== undefined) {
				w.field(7)
				w.uint(v.connectionCredit)
				n++
			}
			if (v.streamCredit !== undefined) {
				w.field(8)
				w.uint(v.streamCredit)
				n++
			}
			if (v.engines !== undefined) {
				w.field(9)
				w.array(v.engines.length)
				for (const item0 of v.engines) {
					w.str(item0)
				}
				n++
			}
			if (v.now !== undefined) {
				w.field(10)
				w.int(v.now)
				n++
			}
			if (v.proof !== undefined) {
				w.field(11)
				w.bin(v.proof)
				n++
			}
			if (v.durability !== undefined) {
				w.field(12)
				w.str(v.durability)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $protocol: number | undefined
			let $server: string | undefined
			let $instance: Uint8Array | undefined
			let $capability: string | undefined
			let $maxBody: number | undefined
			let $inFlight: number | undefined
			let $connectionCredit: number | undefined
			let $streamCredit: number | undefined
			let $engines: string[] | undefined
			let $now: number | undefined
			let $proof: Uint8Array | undefined
			let $durability: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$protocol = r.uint()
						break
					case 2:
						$server = r.str()
						break
					case 3:
						$instance = r.bytes()
						break
					case 4:
						$capability = r.str()
						break
					case 5:
						$maxBody = r.uint()
						break
					case 6:
						$inFlight = r.uint()
						break
					case 7:
						$connectionCredit = r.uint()
						break
					case 8:
						$streamCredit = r.uint()
						break
					case 9: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$engines = items0
						break
					}
					case 10:
						$now = r.int()
						break
					case 11:
						$proof = r.bytes()
						break
					case 12:
						$durability = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				protocol: $protocol,
				server: $server,
				instance: $instance,
				capability: $capability,
				maxBody: $maxBody,
				inFlight: $inFlight,
				connectionCredit: $connectionCredit,
				streamCredit: $streamCredit,
				engines: $engines,
				now: $now,
				proof: $proof,
				durability: $durability,
			}
		},
	},
)

/**
 * How a host opens the store in its own process: the pipe's open takes it,
 * and a server reads the same from its command line.
 */
export const StoreOptions = message(
	'store.Options',
	{
		/** full or os: how far a commit of the store's files goes before it returns; each engine's own when absent */
		durability: [1, str],
		/** the file of the store's encryption key, which the store only reads; encryption.key of its directory, made when needed, when absent */
		encryptionKeyFile: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.durability !== undefined) {
				w.field(1)
				w.str(v.durability)
				n++
			}
			if (v.encryptionKeyFile !== undefined) {
				w.field(2)
				w.str(v.encryptionKeyFile)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $durability: string | undefined
			let $encryptionKeyFile: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$durability = r.str()
						break
					case 2:
						$encryptionKeyFile = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { durability: $durability, encryptionKeyFile: $encryptionKeyFile }
		},
	},
)

/**
 * The last frame of a connection.
 */
export const GoAway = message(
	'GoAway',
	{
		code: [1, str],
		message: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.code !== undefined) {
				w.field(1)
				w.str(v.code)
				n++
			}
			if (v.message !== undefined) {
				w.field(2)
				w.str(v.message)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $code: string | undefined
			let $message: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$code = r.str()
						break
					case 2:
						$message = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { code: $code, message: $message }
		},
	},
)

/**
 * A stream's last frame when it failed, with ERROR set: its kind's code, what
 * failed, and the item it names. A message refused is one too.
 */
export const Failure = message(
	'Failure',
	{
		code: [1, str],
		message: [2, str],
		what: [3, names(str)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.code !== undefined) {
				w.field(1)
				w.str(v.code)
				n++
			}
			if (v.message !== undefined) {
				w.field(2)
				w.str(v.message)
				n++
			}
			if (v.what !== undefined) {
				w.field(3)
				writeNames(w, v.what, str.write)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $code: string | undefined
			let $message: string | undefined
			let $what: Record<string, string> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$code = r.str()
						break
					case 2:
						$message = r.str()
						break
					case 3:
						$what = readNames(r, str.read)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { code: $code, message: $message, what: $what }
		},
	},
)

/**
 * What a call that opens something answers: its handle, which later calls name.
 */
export const Handle = message(
	'Handle',
	{
		handle: [1, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle }
		},
	},
)

/**
 * A message with nothing to say.
 */
export const Empty = message(
	'Empty',
	{},
	{
		write(w) {
			w.closeMap(w.openMap(), 0)
		},
		read(r) {
			for (let n = r.message(); n > 0; n--) {
				r.field()
				r.skip()
			}
			r.leave()
			return {}
		},
	},
)

/**
 * How many jobs of a queue run at once: in all, across every worker of the
 * store, and in each group.
 */
export const JobsConcurrency = message(
	'jobs.Concurrency',
	{
		total: [1, uint],
		group: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.total !== undefined) {
				w.field(1)
				w.uint(v.total)
				n++
			}
			if (v.group !== undefined) {
				w.field(2)
				w.uint(v.group)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $total: number | undefined
			let $group: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$total = r.uint()
						break
					case 2:
						$group = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { total: $total, group: $group }
		},
	},
)

/**
 * The wait before a retry: initial, doubling each time up to max.
 */
export const JobsBackoff = message(
	'jobs.Backoff',
	{
		initial: [1, uint],
		max: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.initial !== undefined) {
				w.field(1)
				w.uint(v.initial)
				n++
			}
			if (v.max !== undefined) {
				w.field(2)
				w.uint(v.max)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $initial: number | undefined
			let $max: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$initial = r.uint()
						break
					case 2:
						$max = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { initial: $initial, max: $max }
		},
	},
)

/**
 * Jobs started in any span of per.
 */
export const JobsRate = message(
	'jobs.Rate',
	{
		count: [1, uint],
		per: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.count !== undefined) {
				w.field(1)
				w.uint(v.count)
				n++
			}
			if (v.per !== undefined) {
				w.field(2)
				w.uint(v.per)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $count: number | undefined
			let $per: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$count = r.uint()
						break
					case 2:
						$per = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { count: $count, per: $per }
		},
	},
)

/**
 * Opens a queue; an option left out takes its default.
 */
export const JobsQueueOpen = message(
	'jobs.QueueOpen',
	{
		/** [a-z0-9][a-z0-9_-]{0,63} */
		name: [1, str],
		/** 10 when absent, the first run counted */
		attempts: [2, uint],
		/** 1 s doubling to 1 h when absent */
		backoff: [3, JobsBackoff],
		/** how long one run may take: a minute when absent */
		timeout: [4, uint],
		/** one at a time in each worker when absent */
		concurrency: [5, JobsConcurrency],
		rate: [6, JobsRate],
		/** how long a done job's id stays taken */
		dedupe: [7, uint],
		/** how long a failed job stays: a week when absent */
		keep: [8, uint],
		/** ten million when absent */
		maxWaiting: [9, uint],
		/** a database's handle: the queue lives in its file; jobs.db when absent */
		database: [10, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.attempts !== undefined) {
				w.field(2)
				w.uint(v.attempts)
				n++
			}
			if (v.backoff !== undefined) {
				w.field(3)
				JobsBackoff.write(w, v.backoff)
				n++
			}
			if (v.timeout !== undefined) {
				w.field(4)
				w.uint(v.timeout)
				n++
			}
			if (v.concurrency !== undefined) {
				w.field(5)
				JobsConcurrency.write(w, v.concurrency)
				n++
			}
			if (v.rate !== undefined) {
				w.field(6)
				JobsRate.write(w, v.rate)
				n++
			}
			if (v.dedupe !== undefined) {
				w.field(7)
				w.uint(v.dedupe)
				n++
			}
			if (v.keep !== undefined) {
				w.field(8)
				w.uint(v.keep)
				n++
			}
			if (v.maxWaiting !== undefined) {
				w.field(9)
				w.uint(v.maxWaiting)
				n++
			}
			if (v.database !== undefined) {
				w.field(10)
				w.uint(v.database)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $attempts: number | undefined
			let $backoff: Read<typeof JobsBackoff.fields> | undefined
			let $timeout: number | undefined
			let $concurrency: Read<typeof JobsConcurrency.fields> | undefined
			let $rate: Read<typeof JobsRate.fields> | undefined
			let $dedupe: number | undefined
			let $keep: number | undefined
			let $maxWaiting: number | undefined
			let $database: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$attempts = r.uint()
						break
					case 3:
						$backoff = JobsBackoff.read(r)
						break
					case 4:
						$timeout = r.uint()
						break
					case 5:
						$concurrency = JobsConcurrency.read(r)
						break
					case 6:
						$rate = JobsRate.read(r)
						break
					case 7:
						$dedupe = r.uint()
						break
					case 8:
						$keep = r.uint()
						break
					case 9:
						$maxWaiting = r.uint()
						break
					case 10:
						$database = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				name: $name,
				attempts: $attempts,
				backoff: $backoff,
				timeout: $timeout,
				concurrency: $concurrency,
				rate: $rate,
				dedupe: $dedupe,
				keep: $keep,
				maxWaiting: $maxWaiting,
				database: $database,
			}
		},
	},
)

/**
 * Opens a schedule: one repeating job the code owns, under its name, whose
 * repeat replaces the one kept.
 */
export const JobsScheduleOpen = message(
	'jobs.ScheduleOpen',
	{
		name: [1, str],
		every: [2, uint],
		/** five fields, with timeZone */
		cron: [3, str],
		/** an IANA name, UTC among them */
		timeZone: [4, str],
		attempts: [5, uint],
		backoff: [6, JobsBackoff],
		timeout: [7, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.every !== undefined) {
				w.field(2)
				w.uint(v.every)
				n++
			}
			if (v.cron !== undefined) {
				w.field(3)
				w.str(v.cron)
				n++
			}
			if (v.timeZone !== undefined) {
				w.field(4)
				w.str(v.timeZone)
				n++
			}
			if (v.attempts !== undefined) {
				w.field(5)
				w.uint(v.attempts)
				n++
			}
			if (v.backoff !== undefined) {
				w.field(6)
				JobsBackoff.write(w, v.backoff)
				n++
			}
			if (v.timeout !== undefined) {
				w.field(7)
				w.uint(v.timeout)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $every: number | undefined
			let $cron: string | undefined
			let $timeZone: string | undefined
			let $attempts: number | undefined
			let $backoff: Read<typeof JobsBackoff.fields> | undefined
			let $timeout: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$every = r.uint()
						break
					case 3:
						$cron = r.str()
						break
					case 4:
						$timeZone = r.str()
						break
					case 5:
						$attempts = r.uint()
						break
					case 6:
						$backoff = JobsBackoff.read(r)
						break
					case 7:
						$timeout = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				name: $name,
				every: $every,
				cron: $cron,
				timeZone: $timeZone,
				attempts: $attempts,
				backoff: $backoff,
				timeout: $timeout,
			}
		},
	},
)

/**
 * A call on one job: an add, a set or an update.
 */
export const JobsCall = message(
	'jobs.Call',
	{
		handle: [1, uint],
		/** 1 to 1024 bytes; a set and an update need one */
		id: [2, str],
		value: [3, str],
		at: [4, int],
		/** from now; not beside at */
		delay: [5, uint],
		group: [6, str],
		/** a repeat, which needs an id */
		every: [7, uint],
		cron: [8, str],
		timeZone: [9, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.id !== undefined) {
				w.field(2)
				w.str(v.id)
				n++
			}
			if (v.value !== undefined) {
				w.field(3)
				w.str(v.value)
				n++
			}
			if (v.at !== undefined) {
				w.field(4)
				w.int(v.at)
				n++
			}
			if (v.delay !== undefined) {
				w.field(5)
				w.uint(v.delay)
				n++
			}
			if (v.group !== undefined) {
				w.field(6)
				w.str(v.group)
				n++
			}
			if (v.every !== undefined) {
				w.field(7)
				w.uint(v.every)
				n++
			}
			if (v.cron !== undefined) {
				w.field(8)
				w.str(v.cron)
				n++
			}
			if (v.timeZone !== undefined) {
				w.field(9)
				w.str(v.timeZone)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $id: string | undefined
			let $value: string | undefined
			let $at: number | undefined
			let $delay: number | undefined
			let $group: string | undefined
			let $every: number | undefined
			let $cron: string | undefined
			let $timeZone: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$id = r.str()
						break
					case 3:
						$value = r.str()
						break
					case 4:
						$at = r.int()
						break
					case 5:
						$delay = r.uint()
						break
					case 6:
						$group = r.str()
						break
					case 7:
						$every = r.uint()
						break
					case 8:
						$cron = r.str()
						break
					case 9:
						$timeZone = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				handle: $handle,
				id: $id,
				value: $value,
				at: $at,
				delay: $delay,
				group: $group,
				every: $every,
				cron: $cron,
				timeZone: $timeZone,
			}
		},
	},
)

/**
 * The job under an id.
 */
export const JobsId = message(
	'jobs.Id',
	{
		handle: [1, uint],
		id: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.id !== undefined) {
				w.field(2)
				w.str(v.id)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $id: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$id = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, id: $id }
		},
	},
)

/**
 * Whether a call did what it asked: an add added, an update changed, a cancel
 * found a job.
 */
export const JobsChanged = message(
	'jobs.Changed',
	{
		changed: [1, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.changed !== undefined) {
				w.field(1)
				w.bool(v.changed)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $changed: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$changed = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { changed: $changed }
		},
	},
)

/**
 * A job as its queue holds it.
 */
export const JobsJob = message(
	'jobs.Job',
	{
		found: [1, bool],
		id: [2, str],
		value: [3, str],
		/** scheduled, waiting, running, done, failed or cancelled */
		state: [4, str],
		/** when it runs next; for one done or failed, when its last run was for */
		at: [5, int],
		attempt: [6, uint],
		/** the jobs that run before a waiting one, up to 10,000 */
		ahead: [7, uint],
		progress: [8, str],
		error: [9, str],
		group: [10, str],
		/** as the job keeps it: @every 30s +6178ms, 10 3 * * * Europe/Berlin */
		repeat: [11, str],
		/** the last run a handler finished */
		startedAt: [12, int],
		endedAt: [13, int],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.found !== undefined) {
				w.field(1)
				w.bool(v.found)
				n++
			}
			if (v.id !== undefined) {
				w.field(2)
				w.str(v.id)
				n++
			}
			if (v.value !== undefined) {
				w.field(3)
				w.str(v.value)
				n++
			}
			if (v.state !== undefined) {
				w.field(4)
				w.str(v.state)
				n++
			}
			if (v.at !== undefined) {
				w.field(5)
				w.int(v.at)
				n++
			}
			if (v.attempt !== undefined) {
				w.field(6)
				w.uint(v.attempt)
				n++
			}
			if (v.ahead !== undefined) {
				w.field(7)
				w.uint(v.ahead)
				n++
			}
			if (v.progress !== undefined) {
				w.field(8)
				w.str(v.progress)
				n++
			}
			if (v.error !== undefined) {
				w.field(9)
				w.str(v.error)
				n++
			}
			if (v.group !== undefined) {
				w.field(10)
				w.str(v.group)
				n++
			}
			if (v.repeat !== undefined) {
				w.field(11)
				w.str(v.repeat)
				n++
			}
			if (v.startedAt !== undefined) {
				w.field(12)
				w.int(v.startedAt)
				n++
			}
			if (v.endedAt !== undefined) {
				w.field(13)
				w.int(v.endedAt)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $found: boolean | undefined
			let $id: string | undefined
			let $value: string | undefined
			let $state: string | undefined
			let $at: number | undefined
			let $attempt: number | undefined
			let $ahead: number | undefined
			let $progress: string | undefined
			let $error: string | undefined
			let $group: string | undefined
			let $repeat: string | undefined
			let $startedAt: number | undefined
			let $endedAt: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$found = r.bool()
						break
					case 2:
						$id = r.str()
						break
					case 3:
						$value = r.str()
						break
					case 4:
						$state = r.str()
						break
					case 5:
						$at = r.int()
						break
					case 6:
						$attempt = r.uint()
						break
					case 7:
						$ahead = r.uint()
						break
					case 8:
						$progress = r.str()
						break
					case 9:
						$error = r.str()
						break
					case 10:
						$group = r.str()
						break
					case 11:
						$repeat = r.str()
						break
					case 12:
						$startedAt = r.int()
						break
					case 13:
						$endedAt = r.int()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				found: $found,
				id: $id,
				value: $value,
				state: $state,
				at: $at,
				attempt: $attempt,
				ahead: $ahead,
				progress: $progress,
				error: $error,
				group: $group,
				repeat: $repeat,
				startedAt: $startedAt,
				endedAt: $endedAt,
			}
		},
	},
)

/**
 * A page of a queue's jobs: those whose ids start with a prefix, in the byte
 * order of their ids, or with no prefix and the failed state, the last failed
 * first.
 */
export const JobsList = message(
	'jobs.List',
	{
		handle: [1, uint],
		prefix: [2, str],
		/** scheduled, waiting, running or failed */
		state: [3, str],
		/** the page before's next */
		after: [4, str],
		/** 100 when absent, 1000 at most */
		limit: [5, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.prefix !== undefined) {
				w.field(2)
				w.str(v.prefix)
				n++
			}
			if (v.state !== undefined) {
				w.field(3)
				w.str(v.state)
				n++
			}
			if (v.after !== undefined) {
				w.field(4)
				w.str(v.after)
				n++
			}
			if (v.limit !== undefined) {
				w.field(5)
				w.uint(v.limit)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $prefix: string | undefined
			let $state: string | undefined
			let $after: string | undefined
			let $limit: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$prefix = r.str()
						break
					case 3:
						$state = r.str()
						break
					case 4:
						$after = r.str()
						break
					case 5:
						$limit = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, prefix: $prefix, state: $state, after: $after, limit: $limit }
		},
	},
)

export const JobsPage = message(
	'jobs.Page',
	{
		jobs: [1, list(JobsJob)],
		next: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.jobs !== undefined) {
				w.field(1)
				w.array(v.jobs.length)
				for (const item0 of v.jobs) {
					JobsJob.write(w, item0)
				}
				n++
			}
			if (v.next !== undefined) {
				w.field(2)
				w.str(v.next)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $jobs: Read<typeof JobsJob.fields>[] | undefined
			let $next: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof JobsJob.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(JobsJob.read(r))
						}
						r.leave()
						$jobs = items0
						break
					}
					case 2:
						$next = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { jobs: $jobs, next: $next }
		},
	},
)

/**
 * Starts the queue's worker for the client: the server claims its jobs as they
 * fall due and hands each over as a held job, no more at once than the
 * client's handlers, and the client answers each. A stop, or the server's
 * GOAWAY, hands no job more, and the server ends the stream with DATA·END once
 * the jobs the client holds are answered and written. The client's DATA·END
 * ends the worker at once, the attempt of a job it still holds failing.
 */
export const JobsWork = message(
	'jobs.Work',
	{
		handle: [1, uint],
		/** the handlers the client runs at once, 1024 at most: the queue's total, or one */
		concurrency: [2, uint],
		/** runDue: the server ends the stream once no job is due and none is held */
		untilIdle: [3, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.concurrency !== undefined) {
				w.field(2)
				w.uint(v.concurrency)
				n++
			}
			if (v.untilIdle !== undefined) {
				w.field(3)
				w.bool(v.untilIdle)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $concurrency: number | undefined
			let $untilIdle: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$concurrency = r.uint()
						break
					case 3:
						$untilIdle = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, concurrency: $concurrency, untilIdle: $untilIdle }
		},
	},
)

/**
 * A job the server hands the client's worker. Its run's number names it to the
 * answer, a step and a keep; a cancel sends it again, cancelled, so that its
 * handler stops.
 */
export const JobsHeld = message(
	'jobs.Held',
	{
		run: [1, uint],
		id: [2, str],
		value: [3, str],
		/** when the run was due */
		at: [4, int],
		/** the first being 1 */
		attempt: [5, uint],
		group: [6, str],
		cancelled: [7, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.run !== undefined) {
				w.field(1)
				w.uint(v.run)
				n++
			}
			if (v.id !== undefined) {
				w.field(2)
				w.str(v.id)
				n++
			}
			if (v.value !== undefined) {
				w.field(3)
				w.str(v.value)
				n++
			}
			if (v.at !== undefined) {
				w.field(4)
				w.int(v.at)
				n++
			}
			if (v.attempt !== undefined) {
				w.field(5)
				w.uint(v.attempt)
				n++
			}
			if (v.group !== undefined) {
				w.field(6)
				w.str(v.group)
				n++
			}
			if (v.cancelled !== undefined) {
				w.field(7)
				w.bool(v.cancelled)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $run: number | undefined
			let $id: string | undefined
			let $value: string | undefined
			let $at: number | undefined
			let $attempt: number | undefined
			let $group: string | undefined
			let $cancelled: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$run = r.uint()
						break
					case 2:
						$id = r.str()
						break
					case 3:
						$value = r.str()
						break
					case 4:
						$at = r.int()
						break
					case 5:
						$attempt = r.uint()
						break
					case 6:
						$group = r.str()
						break
					case 7:
						$cancelled = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				run: $run,
				id: $id,
				value: $value,
				at: $at,
				attempt: $attempt,
				group: $group,
				cancelled: $cancelled,
			}
		},
	},
)

/**
 * The client's answer for a held job: how its run ended, or how far it got,
 * which settles nothing; or a stop, which names no run. An answer for a job a
 * cancel took settles nothing.
 */
export const JobsAnswer = message(
	'jobs.Answer',
	{
		run: [1, uint],
		/** done, retry, snooze, fail, back, progress or stop */
		how: [2, str],
		/** when a retry or a snooze runs again; a retry without one waits its backoff */
		at: [3, int],
		/** why a retry or a failure */
		error: [4, str],
		/** 4 KiB at most */
		progress: [5, str],
		/** from now on the server's clock; not beside at */
		delay: [6, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.run !== undefined) {
				w.field(1)
				w.uint(v.run)
				n++
			}
			if (v.how !== undefined) {
				w.field(2)
				w.str(v.how)
				n++
			}
			if (v.at !== undefined) {
				w.field(3)
				w.int(v.at)
				n++
			}
			if (v.error !== undefined) {
				w.field(4)
				w.str(v.error)
				n++
			}
			if (v.progress !== undefined) {
				w.field(5)
				w.str(v.progress)
				n++
			}
			if (v.delay !== undefined) {
				w.field(6)
				w.uint(v.delay)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $run: number | undefined
			let $how: string | undefined
			let $at: number | undefined
			let $error: string | undefined
			let $progress: string | undefined
			let $delay: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$run = r.uint()
						break
					case 2:
						$how = r.str()
						break
					case 3:
						$at = r.int()
						break
					case 4:
						$error = r.str()
						break
					case 5:
						$progress = r.str()
						break
					case 6:
						$delay = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { run: $run, how: $how, at: $at, error: $error, progress: $progress, delay: $delay }
		},
	},
)

/**
 * A step of a held job's run: jobs.step asks for its kept answer, jobs.keep
 * keeps one.
 */
export const JobsStep = message(
	'jobs.Step',
	{
		run: [1, uint],
		/** 1 to 256 bytes */
		name: [2, str],
		/** jobs.keep's: 1 MiB at most */
		answer: [3, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.run !== undefined) {
				w.field(1)
				w.uint(v.run)
				n++
			}
			if (v.name !== undefined) {
				w.field(2)
				w.str(v.name)
				n++
			}
			if (v.answer !== undefined) {
				w.field(3)
				w.str(v.answer)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $run: number | undefined
			let $name: string | undefined
			let $answer: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$run = r.uint()
						break
					case 2:
						$name = r.str()
						break
					case 3:
						$answer = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { run: $run, name: $name, answer: $answer }
		},
	},
)

export const JobsKept = message(
	'jobs.Kept',
	{
		found: [1, bool],
		answer: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.found !== undefined) {
				w.field(1)
				w.bool(v.found)
				n++
			}
			if (v.answer !== undefined) {
				w.field(2)
				w.str(v.answer)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $found: boolean | undefined
			let $answer: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$found = r.bool()
						break
					case 2:
						$answer = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { found: $found, answer: $answer }
		},
	},
)

/**
 * One write of a transaction: an add, a set or an update with its call, or a
 * cancel with its id.
 */
export const JobsOp = message(
	'jobs.Op',
	{
		/** jobs.add, jobs.set, jobs.update or jobs.cancel */
		method: [1, uint],
		call: [2, JobsCall],
		id: [3, JobsId],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.method !== undefined) {
				w.field(1)
				w.uint(v.method)
				n++
			}
			if (v.call !== undefined) {
				w.field(2)
				JobsCall.write(w, v.call)
				n++
			}
			if (v.id !== undefined) {
				w.field(3)
				JobsId.write(w, v.id)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $method: number | undefined
			let $call: Read<typeof JobsCall.fields> | undefined
			let $id: Read<typeof JobsId.fields> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$method = r.uint()
						break
					case 2:
						$call = JobsCall.read(r)
						break
					case 3:
						$id = JobsId.read(r)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { method: $method, call: $call, id: $id }
		},
	},
)

/**
 * A transaction of jobs.db across the wire: its writes, all applied or none.
 * One that fails names its place in what, as write.
 */
export const JobsTx = message(
	'jobs.Tx',
	{
		writes: [1, list(JobsOp)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.writes !== undefined) {
				w.field(1)
				w.array(v.writes.length)
				for (const item0 of v.writes) {
					JobsOp.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $writes: Read<typeof JobsOp.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof JobsOp.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(JobsOp.read(r))
						}
						r.leave()
						$writes = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { writes: $writes }
		},
	},
)

/**
 * What each write of a transaction did, in their order: whether an add added,
 * an update changed or a cancel found a job; a set changes always.
 */
export const JobsTxResults = message(
	'jobs.TxResults',
	{
		outcomes: [1, list(JobsChanged)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.outcomes !== undefined) {
				w.field(1)
				w.array(v.outcomes.length)
				for (const item0 of v.outcomes) {
					JobsChanged.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $outcomes: Read<typeof JobsChanged.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof JobsChanged.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(JobsChanged.read(r))
						}
						r.leave()
						$outcomes = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { outcomes: $outcomes }
		},
	},
)

/**
 * Opens a bucket of values by key. Its keys expire ttl after they are written,
 * or idle after they were last read or written; not both.
 */
export const KvBucketOpen = message(
	'kv.BucketOpen',
	{
		/** [a-z0-9][a-z0-9_-]{0,63} */
		name: [1, str],
		ttl: [2, uint],
		idle: [3, uint],
		/** a database's handle: the bucket lives in its file; kv.db when absent */
		database: [4, uint],
		/** its values are sealed with the server's encryption key; a read connection opens none */
		encrypted: [5, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.ttl !== undefined) {
				w.field(2)
				w.uint(v.ttl)
				n++
			}
			if (v.idle !== undefined) {
				w.field(3)
				w.uint(v.idle)
				n++
			}
			if (v.database !== undefined) {
				w.field(4)
				w.uint(v.database)
				n++
			}
			if (v.encrypted !== undefined) {
				w.field(5)
				w.bool(v.encrypted)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $ttl: number | undefined
			let $idle: number | undefined
			let $database: number | undefined
			let $encrypted: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$ttl = r.uint()
						break
					case 3:
						$idle = r.uint()
						break
					case 4:
						$database = r.uint()
						break
					case 5:
						$encrypted = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, ttl: $ttl, idle: $idle, database: $database, encrypted: $encrypted }
		},
	},
)

/**
 * A call on one key of a handle's branch.
 */
export const KvCall = message(
	'kv.Call',
	{
		handle: [1, uint],
		/** the branch's owners, outermost first; the bucket's root when empty */
		under: [2, list(key)],
		key: [3, key],
		/** what a set or a create writes */
		value: [4, kvValue],
		/** the expiry a write gives, from now */
		ttl: [5, uint],
		/** the expiry a write gives, as a time */
		expiresAt: [6, int],
		/** the version the key must still be at */
		ifVersion: [7, bin],
		/** what a counter adds; the requests or uses a limit is asked for, 1 when absent */
		n: [8, int64],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.under !== undefined) {
				w.field(2)
				w.array(v.under.length)
				for (const item0 of v.under) {
					key.write(w, item0)
				}
				n++
			}
			if (v.key !== undefined) {
				w.field(3)
				key.write(w, v.key)
				n++
			}
			if (v.value !== undefined) {
				w.field(4)
				kvValue.write(w, v.value)
				n++
			}
			if (v.ttl !== undefined) {
				w.field(5)
				w.uint(v.ttl)
				n++
			}
			if (v.expiresAt !== undefined) {
				w.field(6)
				w.int(v.expiresAt)
				n++
			}
			if (v.ifVersion !== undefined) {
				w.field(7)
				w.bin(v.ifVersion)
				n++
			}
			if (v.n !== undefined) {
				w.field(8)
				w.int(v.n)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $under: (string | Uint8Array)[] | undefined
			let $key: string | Uint8Array | undefined
			let $value: Raw | undefined
			let $ttl: number | undefined
			let $expiresAt: number | undefined
			let $ifVersion: Uint8Array | undefined
			let $n: bigint | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: (string | Uint8Array)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(key.read(r))
						}
						r.leave()
						$under = items0
						break
					}
					case 3:
						$key = key.read(r)
						break
					case 4:
						$value = kvValue.read(r)
						break
					case 5:
						$ttl = r.uint()
						break
					case 6:
						$expiresAt = r.int()
						break
					case 7:
						$ifVersion = r.bytes()
						break
					case 8:
						$n = r.int64()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				handle: $handle,
				under: $under,
				key: $key,
				value: $value,
				ttl: $ttl,
				expiresAt: $expiresAt,
				ifVersion: $ifVersion,
				n: $n,
			}
		},
	},
)

/**
 * A key's value with what a conditional write needs.
 */
export const KvEntry = message(
	'kv.Entry',
	{
		found: [1, bool],
		value: [2, kvValue],
		/** compared only for equality */
		version: [3, bin],
		/** absent for a key that never expires */
		expiresAt: [4, int],
		/** a page's entry alone */
		key: [5, key],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.found !== undefined) {
				w.field(1)
				w.bool(v.found)
				n++
			}
			if (v.value !== undefined) {
				w.field(2)
				kvValue.write(w, v.value)
				n++
			}
			if (v.version !== undefined) {
				w.field(3)
				w.bin(v.version)
				n++
			}
			if (v.expiresAt !== undefined) {
				w.field(4)
				w.int(v.expiresAt)
				n++
			}
			if (v.key !== undefined) {
				w.field(5)
				key.write(w, v.key)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $found: boolean | undefined
			let $value: Raw | undefined
			let $version: Uint8Array | undefined
			let $expiresAt: number | undefined
			let $key: string | Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$found = r.bool()
						break
					case 2:
						$value = kvValue.read(r)
						break
					case 3:
						$version = r.bytes()
						break
					case 4:
						$expiresAt = r.int()
						break
					case 5:
						$key = key.read(r)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { found: $found, value: $value, version: $version, expiresAt: $expiresAt, key: $key }
		},
	},
)

/**
 * What a write left: whether it wrote, and the version and expiry the key has
 * now, the live key's own when a create found one.
 */
export const KvWritten = message(
	'kv.Written',
	{
		written: [1, bool],
		version: [2, bin],
		expiresAt: [3, int],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.written !== undefined) {
				w.field(1)
				w.bool(v.written)
				n++
			}
			if (v.version !== undefined) {
				w.field(2)
				w.bin(v.version)
				n++
			}
			if (v.expiresAt !== undefined) {
				w.field(3)
				w.int(v.expiresAt)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $written: boolean | undefined
			let $version: Uint8Array | undefined
			let $expiresAt: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$written = r.bool()
						break
					case 2:
						$version = r.bytes()
						break
					case 3:
						$expiresAt = r.int()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { written: $written, version: $version, expiresAt: $expiresAt }
		},
	},
)

export const KvFound = message(
	'kv.Found',
	{
		found: [1, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.found !== undefined) {
				w.field(1)
				w.bool(v.found)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $found: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$found = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { found: $found }
		},
	},
)

/**
 * A branch of a handle, for a clear.
 */
export const KvBranch = message(
	'kv.Branch',
	{
		handle: [1, uint],
		under: [2, list(key)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.under !== undefined) {
				w.field(2)
				w.array(v.under.length)
				for (const item0 of v.under) {
					key.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $under: (string | Uint8Array)[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: (string | Uint8Array)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(key.read(r))
						}
						r.leave()
						$under = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, under: $under }
		},
	},
)

/**
 * A page of a branch's own keys, in the byte order of their text.
 */
export const KvList = message(
	'kv.List',
	{
		handle: [1, uint],
		under: [2, list(key)],
		/** the key the page starts after */
		after: [3, key],
		/** 100 when absent, 1000 at most */
		limit: [4, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.under !== undefined) {
				w.field(2)
				w.array(v.under.length)
				for (const item0 of v.under) {
					key.write(w, item0)
				}
				n++
			}
			if (v.after !== undefined) {
				w.field(3)
				key.write(w, v.after)
				n++
			}
			if (v.limit !== undefined) {
				w.field(4)
				w.uint(v.limit)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $under: (string | Uint8Array)[] | undefined
			let $after: string | Uint8Array | undefined
			let $limit: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: (string | Uint8Array)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(key.read(r))
						}
						r.leave()
						$under = items0
						break
					}
					case 3:
						$after = key.read(r)
						break
					case 4:
						$limit = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, under: $under, after: $after, limit: $limit }
		},
	},
)

/**
 * A page of entries, as many as the limit asks and the body holds, and where
 * the next page starts, when there is one.
 */
export const KvPage = message(
	'kv.Page',
	{
		entries: [1, list(KvEntry)],
		next: [2, key],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.entries !== undefined) {
				w.field(1)
				w.array(v.entries.length)
				for (const item0 of v.entries) {
					KvEntry.write(w, item0)
				}
				n++
			}
			if (v.next !== undefined) {
				w.field(2)
				key.write(w, v.next)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $entries: Read<typeof KvEntry.fields>[] | undefined
			let $next: string | Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof KvEntry.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(KvEntry.read(r))
						}
						r.leave()
						$entries = items0
						break
					}
					case 2:
						$next = key.read(r)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { entries: $entries, next: $next }
		},
	},
)

/**
 * Opens counters: numbers by key that only add up.
 */
export const KvCountersOpen = message(
	'kv.CountersOpen',
	{
		name: [1, str],
		/** a counter lasts this long from its first add */
		ttl: [2, uint],
		/** kept in memory and written every span; each add written when absent */
		flushEvery: [3, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.ttl !== undefined) {
				w.field(2)
				w.uint(v.ttl)
				n++
			}
			if (v.flushEvery !== undefined) {
				w.field(3)
				w.uint(v.flushEvery)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $ttl: number | undefined
			let $flushEvery: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$ttl = r.uint()
						break
					case 3:
						$flushEvery = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, ttl: $ttl, flushEvery: $flushEvery }
		},
	},
)

export const KvCount = message(
	'kv.Count',
	{
		value: [1, int64],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.value !== undefined) {
				w.field(1)
				w.int(v.value)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $value: bigint | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$value = r.int64()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { value: $value }
		},
	},
)

/**
 * Opens a rate limit: rate requests a key every per, burst at once.
 */
export const KvRateLimitOpen = message(
	'kv.RateLimitOpen',
	{
		name: [1, str],
		rate: [2, uint],
		per: [3, uint],
		/** the rate when absent */
		burst: [4, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.rate !== undefined) {
				w.field(2)
				w.uint(v.rate)
				n++
			}
			if (v.per !== undefined) {
				w.field(3)
				w.uint(v.per)
				n++
			}
			if (v.burst !== undefined) {
				w.field(4)
				w.uint(v.burst)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $rate: number | undefined
			let $per: number | undefined
			let $burst: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$rate = r.uint()
						break
					case 3:
						$per = r.uint()
						break
					case 4:
						$burst = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, rate: $rate, per: $per, burst: $burst }
		},
	},
)

/**
 * One window of a quota: a key may use up to limit every per from its first use.
 */
export const KvWindow = message(
	'kv.Window',
	{
		/** [a-z][a-z0-9_]{0,31} */
		name: [1, str],
		limit: [2, uint],
		per: [3, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.limit !== undefined) {
				w.field(2)
				w.uint(v.limit)
				n++
			}
			if (v.per !== undefined) {
				w.field(3)
				w.uint(v.per)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $limit: number | undefined
			let $per: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$limit = r.uint()
						break
					case 3:
						$per = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, limit: $limit, per: $per }
		},
	},
)

export const KvQuotaOpen = message(
	'kv.QuotaOpen',
	{
		name: [1, str],
		/** one to eight */
		windows: [2, list(KvWindow)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.windows !== undefined) {
				w.field(2)
				w.array(v.windows.length)
				for (const item0 of v.windows) {
					KvWindow.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $windows: Read<typeof KvWindow.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2: {
						const items0: Read<typeof KvWindow.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(KvWindow.read(r))
						}
						r.leave()
						$windows = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, windows: $windows }
		},
	},
)

/**
 * One window of a quota as a key stands in it.
 */
export const KvWindowUse = message(
	'kv.WindowUse',
	{
		name: [1, str],
		used: [2, uint],
		limit: [3, uint],
		left: [4, uint],
		/** absent for a window not started */
		resetsAt: [5, int],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.used !== undefined) {
				w.field(2)
				w.uint(v.used)
				n++
			}
			if (v.limit !== undefined) {
				w.field(3)
				w.uint(v.limit)
				n++
			}
			if (v.left !== undefined) {
				w.field(4)
				w.uint(v.left)
				n++
			}
			if (v.resetsAt !== undefined) {
				w.field(5)
				w.int(v.resetsAt)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $used: number | undefined
			let $limit: number | undefined
			let $left: number | undefined
			let $resetsAt: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$used = r.uint()
						break
					case 3:
						$limit = r.uint()
						break
					case 4:
						$left = r.uint()
						break
					case 5:
						$resetsAt = r.int()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, used: $used, limit: $limit, left: $left, resetsAt: $resetsAt }
		},
	},
)

/**
 * What a rate limit or a quota answers a request.
 */
export const KvAllowance = message(
	'kv.Allowance',
	{
		ok: [1, bool],
		/** how many more would pass now */
		left: [2, uint],
		/** when the request would pass; absent when it did */
		retryAt: [3, int],
		/** a quota's, in the order it names them */
		windows: [4, list(KvWindowUse)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.ok !== undefined) {
				w.field(1)
				w.bool(v.ok)
				n++
			}
			if (v.left !== undefined) {
				w.field(2)
				w.uint(v.left)
				n++
			}
			if (v.retryAt !== undefined) {
				w.field(3)
				w.int(v.retryAt)
				n++
			}
			if (v.windows !== undefined) {
				w.field(4)
				w.array(v.windows.length)
				for (const item0 of v.windows) {
					KvWindowUse.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $ok: boolean | undefined
			let $left: number | undefined
			let $retryAt: number | undefined
			let $windows: Read<typeof KvWindowUse.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$ok = r.bool()
						break
					case 2:
						$left = r.uint()
						break
					case 3:
						$retryAt = r.int()
						break
					case 4: {
						const items0: Read<typeof KvWindowUse.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(KvWindowUse.read(r))
						}
						r.leave()
						$windows = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { ok: $ok, left: $left, retryAt: $retryAt, windows: $windows }
		},
	},
)

/**
 * Opens once's answers: a function run once a key, its answer kept.
 */
export const KvOnceOpen = message(
	'kv.OnceOpen',
	{
		name: [1, str],
		/** a day when absent */
		keep: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.keep !== undefined) {
				w.field(2)
				w.uint(v.keep)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $keep: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$keep = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, keep: $keep }
		},
	},
)

/**
 * An answer of once: in the server's RESPONSE, found when one was kept, and
 * otherwise the run is the client's; in the client's last DATA, found with the
 * answer to keep, or not found when the function failed.
 */
export const KvAnswer = message(
	'kv.Answer',
	{
		found: [1, bool],
		value: [2, kvValue],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.found !== undefined) {
				w.field(1)
				w.bool(v.found)
				n++
			}
			if (v.value !== undefined) {
				w.field(2)
				kvValue.write(w, v.value)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $found: boolean | undefined
			let $value: Raw | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$found = r.bool()
						break
					case 2:
						$value = kvValue.read(r)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { found: $found, value: $value }
		},
	},
)

/**
 * A read a transaction made, which its commit checks: the key still at the
 * version it was found at, or still absent.
 */
export const KvCheck = message(
	'kv.Check',
	{
		handle: [1, uint],
		under: [2, list(key)],
		key: [3, key],
		/** absent: the read found nothing */
		version: [4, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.under !== undefined) {
				w.field(2)
				w.array(v.under.length)
				for (const item0 of v.under) {
					key.write(w, item0)
				}
				n++
			}
			if (v.key !== undefined) {
				w.field(3)
				key.write(w, v.key)
				n++
			}
			if (v.version !== undefined) {
				w.field(4)
				w.bin(v.version)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $under: (string | Uint8Array)[] | undefined
			let $key: string | Uint8Array | undefined
			let $version: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: (string | Uint8Array)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(key.read(r))
						}
						r.leave()
						$under = items0
						break
					}
					case 3:
						$key = key.read(r)
						break
					case 4:
						$version = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, under: $under, key: $key, version: $version }
		},
	},
)

/**
 * One write of a transaction: the method it is, and its call.
 */
export const KvOp = message(
	'kv.Op',
	{
		/** kv.set, kv.create, kv.take, kv.delete, kv.expire, kv.clear or kv.counters.add */
		method: [1, uint],
		call: [2, KvCall],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.method !== undefined) {
				w.field(1)
				w.uint(v.method)
				n++
			}
			if (v.call !== undefined) {
				w.field(2)
				KvCall.write(w, v.call)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $method: number | undefined
			let $call: Read<typeof KvCall.fields> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$method = r.uint()
						break
					case 2:
						$call = KvCall.read(r)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { method: $method, call: $call }
		},
	},
)

/**
 * A transaction across the wire: its reads' checks, then its writes, all
 * applied or none. A failed check or write names its place in what.
 */
export const KvTx = message(
	'kv.Tx',
	{
		checks: [1, list(KvCheck)],
		writes: [2, list(KvOp)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.checks !== undefined) {
				w.field(1)
				w.array(v.checks.length)
				for (const item0 of v.checks) {
					KvCheck.write(w, item0)
				}
				n++
			}
			if (v.writes !== undefined) {
				w.field(2)
				w.array(v.writes.length)
				for (const item0 of v.writes) {
					KvOp.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $checks: Read<typeof KvCheck.fields>[] | undefined
			let $writes: Read<typeof KvOp.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof KvCheck.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(KvCheck.read(r))
						}
						r.leave()
						$checks = items0
						break
					}
					case 2: {
						const items0: Read<typeof KvOp.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(KvOp.read(r))
						}
						r.leave()
						$writes = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { checks: $checks, writes: $writes }
		},
	},
)

/**
 * What one write of a transaction did.
 */
export const KvOutcome = message(
	'kv.Outcome',
	{
		/** a take, a delete or an expire found a live key */
		found: [1, bool],
		/** a set or a create wrote */
		written: [2, bool],
		/** what a take took */
		value: [3, kvValue],
		version: [4, bin],
		expiresAt: [5, int],
		/** a counter's value after its add */
		count: [6, int64],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.found !== undefined) {
				w.field(1)
				w.bool(v.found)
				n++
			}
			if (v.written !== undefined) {
				w.field(2)
				w.bool(v.written)
				n++
			}
			if (v.value !== undefined) {
				w.field(3)
				kvValue.write(w, v.value)
				n++
			}
			if (v.version !== undefined) {
				w.field(4)
				w.bin(v.version)
				n++
			}
			if (v.expiresAt !== undefined) {
				w.field(5)
				w.int(v.expiresAt)
				n++
			}
			if (v.count !== undefined) {
				w.field(6)
				w.int(v.count)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $found: boolean | undefined
			let $written: boolean | undefined
			let $value: Raw | undefined
			let $version: Uint8Array | undefined
			let $expiresAt: number | undefined
			let $count: bigint | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$found = r.bool()
						break
					case 2:
						$written = r.bool()
						break
					case 3:
						$value = kvValue.read(r)
						break
					case 4:
						$version = r.bytes()
						break
					case 5:
						$expiresAt = r.int()
						break
					case 6:
						$count = r.int64()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				found: $found,
				written: $written,
				value: $value,
				version: $version,
				expiresAt: $expiresAt,
				count: $count,
			}
		},
	},
)

export const KvTxResults = message(
	'kv.TxResults',
	{
		outcomes: [1, list(KvOutcome)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.outcomes !== undefined) {
				w.field(1)
				w.array(v.outcomes.length)
				for (const item0 of v.outcomes) {
					KvOutcome.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $outcomes: Read<typeof KvOutcome.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof KvOutcome.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(KvOutcome.read(r))
						}
						r.leave()
						$outcomes = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { outcomes: $outcomes }
		},
	},
)

/**
 * A private server's clock: a time to set it to, a span to move it forward by,
 * or neither to read it. The answer is the time it reads once moved.
 */
export const ServerClock = message(
	'server.Clock',
	{
		at: [1, int],
		advance: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.at !== undefined) {
				w.field(1)
				w.int(v.at)
				n++
			}
			if (v.advance !== undefined) {
				w.field(2)
				w.uint(v.advance)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $at: number | undefined
			let $advance: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$at = r.int()
						break
					case 2:
						$advance = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { at: $at, advance: $advance }
		},
	},
)

/**
 * A migration as its file has it: the name that numbers it, and its SQL.
 */
export const SqlMigration = message(
	'sql.Migration',
	{
		/** 0001_notes.sql */
		name: [1, str],
		sql: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.sql !== undefined) {
				w.field(2)
				w.str(v.sql)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $sql: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$sql = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, sql: $sql }
		},
	},
)

/**
 * Opens a database. Migrations, when given, are applied and checked; without
 * them the file opens as it is.
 */
export const SqlOpen = message(
	'sql.Open',
	{
		/** [a-z0-9][a-z0-9_-]{0,63} */
		name: [1, str],
		migrations: [2, list(SqlMigration)],
		/** full or os: how far its commits go before they return; the store's own when absent */
		durability: [3, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.migrations !== undefined) {
				w.field(2)
				w.array(v.migrations.length)
				for (const item0 of v.migrations) {
					SqlMigration.write(w, item0)
				}
				n++
			}
			if (v.durability !== undefined) {
				w.field(3)
				w.str(v.durability)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $migrations: Read<typeof SqlMigration.fields>[] | undefined
			let $durability: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2: {
						const items0: Read<typeof SqlMigration.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(SqlMigration.read(r))
						}
						r.leave()
						$migrations = items0
						break
					}
					case 3:
						$durability = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, migrations: $migrations, durability: $durability }
		},
	},
)

/**
 * A statement's text and the values of its ?s, in order.
 */
export const SqlText = message(
	'sql.Text',
	{
		text: [1, str],
		values: [2, list(sqlValue)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.text !== undefined) {
				w.field(1)
				w.str(v.text)
				n++
			}
			if (v.values !== undefined) {
				w.field(2)
				w.array(v.values.length)
				for (const item0 of v.values) {
					sqlValue.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $text: string | undefined
			let $values: (SqlValue | boolean)[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$text = r.str()
						break
					case 2: {
						const items0: (SqlValue | boolean)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(sqlValue.read(r))
						}
						r.leave()
						$values = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { text: $text, values: $values }
		},
	},
)

/**
 * A statement on a database: a write, or a read answered by its columns.
 */
export const SqlStatement = message(
	'sql.Statement',
	{
		handle: [1, uint],
		text: [2, str],
		values: [3, list(sqlValue)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.text !== undefined) {
				w.field(2)
				w.str(v.text)
				n++
			}
			if (v.values !== undefined) {
				w.field(3)
				w.array(v.values.length)
				for (const item0 of v.values) {
					sqlValue.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $text: string | undefined
			let $values: (SqlValue | boolean)[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$text = r.str()
						break
					case 3: {
						const items0: (SqlValue | boolean)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(sqlValue.read(r))
						}
						r.leave()
						$values = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, text: $text, values: $values }
		},
	},
)

/**
 * What a statement gives back: its rows, one row or none, or one value. A
 * write with returning runs on the writer, its rows given once it is durable.
 */
export const SqlQuery = message(
	'sql.Query',
	{
		handle: [1, uint],
		text: [2, str],
		values: [3, list(sqlValue)],
		/** all, one or scalar */
		want: [4, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.text !== undefined) {
				w.field(2)
				w.str(v.text)
				n++
			}
			if (v.values !== undefined) {
				w.field(3)
				w.array(v.values.length)
				for (const item0 of v.values) {
					sqlValue.write(w, item0)
				}
				n++
			}
			if (v.want !== undefined) {
				w.field(4)
				w.str(v.want)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $text: string | undefined
			let $values: (SqlValue | boolean)[] | undefined
			let $want: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$text = r.str()
						break
					case 3: {
						const items0: (SqlValue | boolean)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(sqlValue.read(r))
						}
						r.leave()
						$values = items0
						break
					}
					case 4:
						$want = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, text: $text, values: $values, want: $want }
		},
	},
)

/**
 * Rows a statement gave, a part of them a message: the first part names the
 * columns, and every row holds a value a column, in their order.
 */
export const SqlRows = message(
	'sql.Rows',
	{
		columns: [1, list(str)],
		rows: [2, list(list(sqlValue))],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.columns !== undefined) {
				w.field(1)
				w.array(v.columns.length)
				for (const item0 of v.columns) {
					w.str(item0)
				}
				n++
			}
			if (v.rows !== undefined) {
				w.field(2)
				w.array(v.rows.length)
				for (const item0 of v.rows) {
					w.array(item0.length)
					for (const item1 of item0) {
						sqlValue.write(w, item1)
					}
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $columns: string[] | undefined
			let $rows: (SqlValue | boolean)[][] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$columns = items0
						break
					}
					case 2: {
						const items0: (SqlValue | boolean)[][] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							const items1: (SqlValue | boolean)[] = []
							for (let i1 = r.array(); i1 > 0; i1--) {
								items1.push(sqlValue.read(r))
							}
							r.leave()
							items0.push(items1)
						}
						r.leave()
						$rows = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { columns: $columns, rows: $rows }
		},
	},
)

/**
 * A column of a statement's rows, a part of it a message, as one of three. A
 * column of INTEGERs alone goes as integers and one of REALs alone as reals,
 * 8 bytes a row, little end first; where a row holds NULL its bit of nulls is
 * set, and its value is 0 among integers and a NaN among reals, which SQLite
 * keeps none of. Any other column goes as values: text, blobs, and INTEGERs
 * beside REALs, neither read as the other.
 */
export const SqlColumn = message(
	'sql.Column',
	{
		name: [1, str],
		/** an int64 a row */
		integers: [2, bin],
		/** a float64 a row */
		reals: [3, bin],
		/** a bit a row, a byte's lowest first, set where the row holds NULL; absent when none does */
		nulls: [4, bin],
		values: [5, list(sqlValue)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.name !== undefined) {
				w.field(1)
				w.str(v.name)
				n++
			}
			if (v.integers !== undefined) {
				w.field(2)
				w.bin(v.integers)
				n++
			}
			if (v.reals !== undefined) {
				w.field(3)
				w.bin(v.reals)
				n++
			}
			if (v.nulls !== undefined) {
				w.field(4)
				w.bin(v.nulls)
				n++
			}
			if (v.values !== undefined) {
				w.field(5)
				w.array(v.values.length)
				for (const item0 of v.values) {
					sqlValue.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $integers: Uint8Array | undefined
			let $reals: Uint8Array | undefined
			let $nulls: Uint8Array | undefined
			let $values: (SqlValue | boolean)[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$integers = r.bytes()
						break
					case 3:
						$reals = r.bytes()
						break
					case 4:
						$nulls = r.bytes()
						break
					case 5: {
						const items0: (SqlValue | boolean)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(sqlValue.read(r))
						}
						r.leave()
						$values = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, integers: $integers, reals: $reals, nulls: $nulls, values: $values }
		},
	},
)

/**
 * Rows a statement gave, by their columns, a part of them a message: every
 * part holds every column, in the statement's order and as the same one of
 * three, for the same rows.
 */
export const SqlColumns = message(
	'sql.Columns',
	{
		/** the rows of this part */
		rows: [1, uint],
		/** the rows of every part */
		total: [2, uint],
		columns: [3, list(SqlColumn)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.rows !== undefined) {
				w.field(1)
				w.uint(v.rows)
				n++
			}
			if (v.total !== undefined) {
				w.field(2)
				w.uint(v.total)
				n++
			}
			if (v.columns !== undefined) {
				w.field(3)
				w.array(v.columns.length)
				for (const item0 of v.columns) {
					SqlColumn.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $rows: number | undefined
			let $total: number | undefined
			let $columns: Read<typeof SqlColumn.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$rows = r.uint()
						break
					case 2:
						$total = r.uint()
						break
					case 3: {
						const items0: Read<typeof SqlColumn.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(SqlColumn.read(r))
						}
						r.leave()
						$columns = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { rows: $rows, total: $total, columns: $columns }
		},
	},
)

/**
 * What a write changed: the rows, and SQLite's rowid of the row it inserted,
 * 0 when it inserted none.
 */
export const SqlDone = message(
	'sql.Done',
	{
		changes: [1, uint],
		lastInsertRowid: [2, int64],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.changes !== undefined) {
				w.field(1)
				w.uint(v.changes)
				n++
			}
			if (v.lastInsertRowid !== undefined) {
				w.field(2)
				w.int(v.lastInsertRowid)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $changes: number | undefined
			let $lastInsertRowid: bigint | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$changes = r.uint()
						break
					case 2:
						$lastInsertRowid = r.int64()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { changes: $changes, lastInsertRowid: $lastInsertRowid }
		},
	},
)

/**
 * Statements known before they run, written as one in a shared commit.
 */
export const SqlBatch = message(
	'sql.Batch',
	{
		handle: [1, uint],
		statements: [2, list(SqlText)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			if (v.statements !== undefined) {
				w.field(2)
				w.array(v.statements.length)
				for (const item0 of v.statements) {
					SqlText.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $statements: Read<typeof SqlText.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: Read<typeof SqlText.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(SqlText.read(r))
						}
						r.leave()
						$statements = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, statements: $statements }
		},
	},
)

export const SqlBatched = message(
	'sql.Batched',
	{
		done: [1, list(SqlDone)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.done !== undefined) {
				w.field(1)
				w.array(v.done.length)
				for (const item0 of v.done) {
					SqlDone.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $done: Read<typeof SqlDone.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof SqlDone.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(SqlDone.read(r))
						}
						r.leave()
						$done = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { done: $done }
		},
	},
)

/**
 * Opens a transaction: it holds the database's writer until the client's last
 * DATA, five seconds at most.
 */
export const SqlTxOpen = message(
	'sql.TxOpen',
	{
		handle: [1, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.handle !== undefined) {
				w.field(1)
				w.uint(v.handle)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle }
		},
	},
)

/**
 * A transaction's call, or its end: the last DATA commits, or not. A call is
 * a statement, or a call of kv or jobs on a bucket or a queue kept in the
 * database's file: a method of theirs with its request, kv.set or jobs.add,
 * which runs in the transaction.
 */
export const SqlTxCall = message(
	'sql.TxCall',
	{
		text: [1, str],
		values: [2, list(sqlValue)],
		/** all, one, scalar or exec; call for a method; nothing in the last */
		want: [3, str],
		/** in the last: true commits, false rolls back */
		commit: [4, bool],
		/** with want call: the method, its request in body */
		method: [5, uint],
		body: [6, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.text !== undefined) {
				w.field(1)
				w.str(v.text)
				n++
			}
			if (v.values !== undefined) {
				w.field(2)
				w.array(v.values.length)
				for (const item0 of v.values) {
					sqlValue.write(w, item0)
				}
				n++
			}
			if (v.want !== undefined) {
				w.field(3)
				w.str(v.want)
				n++
			}
			if (v.commit !== undefined) {
				w.field(4)
				w.bool(v.commit)
				n++
			}
			if (v.method !== undefined) {
				w.field(5)
				w.uint(v.method)
				n++
			}
			if (v.body !== undefined) {
				w.field(6)
				w.bin(v.body)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $text: string | undefined
			let $values: (SqlValue | boolean)[] | undefined
			let $want: string | undefined
			let $commit: boolean | undefined
			let $method: number | undefined
			let $body: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$text = r.str()
						break
					case 2: {
						const items0: (SqlValue | boolean)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(sqlValue.read(r))
						}
						r.leave()
						$values = items0
						break
					}
					case 3:
						$want = r.str()
						break
					case 4:
						$commit = r.bool()
						break
					case 5:
						$method = r.uint()
						break
					case 6:
						$body = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				text: $text,
				values: $values,
				want: $want,
				commit: $commit,
				method: $method,
				body: $body,
			}
		},
	},
)

/**
 * A call's answer: its rows, what it changed, a method's answer, or why it
 * failed, which leaves the transaction as it was before the call.
 */
export const SqlTxAnswer = message(
	'sql.TxAnswer',
	{
		rows: [1, SqlRows],
		done: [2, SqlDone],
		failure: [3, Failure],
		/** what a method answered, as its own answer */
		body: [4, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.rows !== undefined) {
				w.field(1)
				SqlRows.write(w, v.rows)
				n++
			}
			if (v.done !== undefined) {
				w.field(2)
				SqlDone.write(w, v.done)
				n++
			}
			if (v.failure !== undefined) {
				w.field(3)
				Failure.write(w, v.failure)
				n++
			}
			if (v.body !== undefined) {
				w.field(4)
				w.bin(v.body)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $rows: Read<typeof SqlRows.fields> | undefined
			let $done: Read<typeof SqlDone.fields> | undefined
			let $failure: Read<typeof Failure.fields> | undefined
			let $body: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$rows = SqlRows.read(r)
						break
					case 2:
						$done = SqlDone.read(r)
						break
					case 3:
						$failure = Failure.read(r)
						break
					case 4:
						$body = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { rows: $rows, done: $done, failure: $failure, body: $body }
		},
	},
)
