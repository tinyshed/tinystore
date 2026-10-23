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
	'backup.write': 0x0801,
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
	/**
	 * Answers once the store has the message, not once anyone handled it; in a
	 * database's transaction, it is handed over once the transaction commits.
	 */
	'channels.publish': 0x0a01,
	/**
	 * Answers once the subscription is registered, then sends batches within the
	 * client's credit until it cancels: a client that grants none falls behind.
	 */
	'channels.subscribe': 0x0a02,
	'config.open': 0x0701,
	/**
	 * The config's value now, then each time it changes, until the client
	 * cancels: one stream a config a process, whose value every handle and
	 * watcher of it there takes.
	 */
	'config.follow': 0x0702,
	'config.update': 0x0703,
	'config.reset': 0x0704,
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
	 * Answers once the key is held, or held past the wait, `in_use`; a CANCEL
	 * takes the ask out of its line.
	 */
	'locks.hold': 0x0901,
	/**
	 * A lease that ran out or was revoked is `lost`.
	 */
	'locks.renew': 0x0902,
	'locks.release': 0x0903,
	'metrics.read': 0x0601,
	'metrics.latest': 0x0602,
	'metrics.aggregate': 0x0603,
	'metrics.explain': 0x0604,
	'metrics.names': 0x0605,
	'metrics.labelNames': 0x0606,
	'metrics.labelValues': 0x0607,
	'metrics.description': 0x0608,
	'metrics.damaged': 0x0609,
	'metrics.ingest': 0x060a,
	'metrics.describe': 0x060b,
	'metrics.delete': 0x060c,
	'metrics.flush': 0x060d,
	'metrics.instruments': 0x060e,
	'metrics.ingestPrometheus': 0x060f,
	'records.open': 0x0501,
	'records.append': 0x0502,
	'records.list': 0x0503,
	'records.count': 0x0504,
	'records.top': 0x0505,
	'records.aggregate': 0x0506,
	'records.streams': 0x0507,
	'records.fieldNames': 0x0508,
	'records.poll': 0x0509,
	'records.tail': 0x050a,
	'records.lines': 0x050b,
	'records.damaged': 0x050c,
	'records.drop': 0x050d,
	'search.open': 0x0b01,
	'search.set': 0x0b02,
	'search.setMany': 0x0b03,
	/**
	 * Answers whether the index held the document.
	 */
	'search.delete': 0x0b04,
	'search.get': 0x0b05,
	'search.search': 0x0b06,
	'search.status': 0x0b07,
	/**
	 * Drops an index and every document it holds, in the background.
	 */
	'search.dropIndex': 0x0b08,
	'search.list': 0x0b09,
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
 * What a backup takes beside every engine's files: the files of the
 * program's it includes, by their paths inside the store's directory, and
 * whether it includes the store's encryption key.
 */
export const BackupWrite = message(
	'backup.Write',
	{
		include: [1, list(str)],
		includeEncryptionKey: [2, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.include !== undefined) {
				w.field(1)
				w.array(v.include.length)
				for (const item0 of v.include) {
					w.str(item0)
				}
				n++
			}
			if (v.includeEncryptionKey !== undefined) {
				w.field(2)
				w.bool(v.includeEncryptionKey)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $include: string[] | undefined
			let $includeEncryptionKey: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$include = items0
						break
					}
					case 2:
						$includeEncryptionKey = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { include: $include, includeEncryptionKey: $includeEncryptionKey }
		},
	},
)

/**
 * A piece of the zip, in order.
 */
export const BackupPiece = message(
	'backup.Piece',
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
 * What the backup holds, once its zip has gone whole.
 */
export const BackupWritten = message(
	'backup.Written',
	{
		files: [1, uint],
		bytes: [2, uint],
		/** the store seals values with a key the backup does not hold */
		keyLeftOut: [3, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.files !== undefined) {
				w.field(1)
				w.uint(v.files)
				n++
			}
			if (v.bytes !== undefined) {
				w.field(2)
				w.uint(v.bytes)
				n++
			}
			if (v.keyLeftOut !== undefined) {
				w.field(3)
				w.bool(v.keyLeftOut)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $files: number | undefined
			let $bytes: number | undefined
			let $keyLeftOut: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$files = r.uint()
						break
					case 2:
						$bytes = r.uint()
						break
					case 3:
						$keyLeftOut = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { files: $files, bytes: $bytes, keyLeftOut: $keyLeftOut }
		},
	},
)

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
 * A message published on a channel's branch, as docs/guides/channels/README.md has
 * it: every subscription listening there now receives it.
 */
export const ChannelsPublish = message(
	'channels.Publish',
	{
		/** [a-z0-9][a-z0-9_-]{0,63} */
		name: [1, str],
		/** the branch's owners, 1 KiB together at most */
		under: [2, list(str)],
		/** the message, 1 MiB at most */
		value: [3, str],
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
			if (v.under !== undefined) {
				w.field(2)
				w.array(v.under.length)
				for (const item0 of v.under) {
					w.str(item0)
				}
				n++
			}
			if (v.value !== undefined) {
				w.field(3)
				w.str(v.value)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $under: string[] | undefined
			let $value: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$under = items0
						break
					}
					case 3:
						$value = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, under: $under, value: $value }
		},
	},
)

/**
 * A subscription of a branch, or of it and every branch below it.
 */
export const ChannelsSubscribe = message(
	'channels.Subscribe',
	{
		name: [1, str],
		under: [2, list(str)],
		recursive: [3, bool],
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
			if (v.under !== undefined) {
				w.field(2)
				w.array(v.under.length)
				for (const item0 of v.under) {
					w.str(item0)
				}
				n++
			}
			if (v.recursive !== undefined) {
				w.field(3)
				w.bool(v.recursive)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $under: string[] | undefined
			let $recursive: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$under = items0
						break
					}
					case 3:
						$recursive = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, under: $under, recursive: $recursive }
		},
	},
)

export const ChannelsMessage = message(
	'channels.Message',
	{
		/** the branch it was published under */
		under: [1, list(str)],
		value: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.under !== undefined) {
				w.field(1)
				w.array(v.under.length)
				for (const item0 of v.under) {
					w.str(item0)
				}
				n++
			}
			if (v.value !== undefined) {
				w.field(2)
				w.str(v.value)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $under: string[] | undefined
			let $value: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$under = items0
						break
					}
					case 2:
						$value = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { under: $under, value: $value }
		},
	},
)

/**
 * A config a program opens: its declaration and its layers as data, as
 * docs/guides/config/README.md has them, with what the program read itself, a file's
 * text and its variables, since those belong to it and not to the server.
 */
export const ConfigOpen = message(
	'config.Open',
	{
		/** [a-z0-9][a-z0-9_-]{0,63} */
		name: [1, str],
		/** each setting's kind, rules, default and marks */
		declaration: [2, str],
		/** objects, files and the environment, in the order written */
		layers: [3, str],
		/** a kept change that no longer fits refuses the open */
		strict: [4, bool],
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
			if (v.declaration !== undefined) {
				w.field(2)
				w.str(v.declaration)
				n++
			}
			if (v.layers !== undefined) {
				w.field(3)
				w.str(v.layers)
				n++
			}
			if (v.strict !== undefined) {
				w.field(4)
				w.bool(v.strict)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $declaration: string | undefined
			let $layers: string | undefined
			let $strict: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$declaration = r.str()
						break
					case 3:
						$layers = r.str()
						break
					case 4:
						$strict = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, declaration: $declaration, layers: $layers, strict: $strict }
		},
	},
)

/**
 * A setting's line of explain.
 */
export const ConfigLine = message(
	'config.Line',
	{
		path: [1, str],
		/** as JSON, a span spelled, a secret as *** */
		value: [2, str],
		from: [3, str],
		/** the values it went over, newest first */
		over: [4, list(str)],
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
			if (v.value !== undefined) {
				w.field(2)
				w.str(v.value)
				n++
			}
			if (v.from !== undefined) {
				w.field(3)
				w.str(v.from)
				n++
			}
			if (v.over !== undefined) {
				w.field(4)
				w.array(v.over.length)
				for (const item0 of v.over) {
					w.str(item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $path: string | undefined
			let $value: string | undefined
			let $from: string | undefined
			let $over: string[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$path = r.str()
						break
					case 2:
						$value = r.str()
						break
					case 3:
						$from = r.str()
						break
					case 4: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$over = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { path: $path, value: $value, from: $from, over: $over }
		},
	},
)

/**
 * A change of live settings, made on the version the program's value had.
 */
export const ConfigUpdate = message(
	'config.Update',
	{
		handle: [1, uint],
		/** a change made since is a conflict */
		version: [2, int],
		/** the settings it changes, in the declaration's shape */
		changes: [3, str],
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
			if (v.version !== undefined) {
				w.field(2)
				w.int(v.version)
				n++
			}
			if (v.changes !== undefined) {
				w.field(3)
				w.str(v.changes)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $version: number | undefined
			let $changes: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$version = r.int()
						break
					case 3:
						$changes = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, version: $version, changes: $changes }
		},
	},
)

/**
 * Drops the changes kept of a setting or of a group, or of every setting.
 */
export const ConfigReset = message(
	'config.Reset',
	{
		handle: [1, uint],
		/** empty for every change */
		path: [2, str],
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
			if (v.path !== undefined) {
				w.field(2)
				w.str(v.path)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $path: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$path = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, path: $path }
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
		/** makes the store a node, which keeps a log of each file it writes; from or serve make it one too */
		node: [3, bool],
		/** where the node takes the files it copies, host:port, tried in order */
		from: [4, list(str)],
		/** where other nodes take files from this one, host:port */
		serve: [5, str],
		/** the files it copies, as kv.db or sql/app.db; every file but its own records and metrics when empty */
		files: [6, list(str)],
		/** what it says to the node it takes from, and asks of the nodes that take from it */
		token: [7, str],
		/** a write of a file it writes answers once a node copying it holds the commit */
		synchronous: [8, bool],
		/** it is the home of the catalog, which names each file's home */
		catalog: [9, bool],
		/** where other nodes reach it, host:port; the address serve listens on when absent */
		advertise: [10, str],
		/** the nodes, by address or id, a synchronous commit waits for one of; any when empty */
		synchronousWith: [11, list(str)],
		/** plain TCP on links beyond this machine, which anyone on the way reads */
		insecure: [12, bool],
		/** measures the store into its own metrics.db, under names that start with tinystore_ */
		selfMetrics: [13, bool],
		/** with selfMetrics, leaves out the machine it runs on: its process's memory and processor, the system's */
		withoutSystem: [14, bool],
		/** the metrics series the store holds at most; 100,000 when absent */
		maxSeries: [15, uint],
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
			if (v.node !== undefined) {
				w.field(3)
				w.bool(v.node)
				n++
			}
			if (v.from !== undefined) {
				w.field(4)
				w.array(v.from.length)
				for (const item0 of v.from) {
					w.str(item0)
				}
				n++
			}
			if (v.serve !== undefined) {
				w.field(5)
				w.str(v.serve)
				n++
			}
			if (v.files !== undefined) {
				w.field(6)
				w.array(v.files.length)
				for (const item0 of v.files) {
					w.str(item0)
				}
				n++
			}
			if (v.token !== undefined) {
				w.field(7)
				w.str(v.token)
				n++
			}
			if (v.synchronous !== undefined) {
				w.field(8)
				w.bool(v.synchronous)
				n++
			}
			if (v.catalog !== undefined) {
				w.field(9)
				w.bool(v.catalog)
				n++
			}
			if (v.advertise !== undefined) {
				w.field(10)
				w.str(v.advertise)
				n++
			}
			if (v.synchronousWith !== undefined) {
				w.field(11)
				w.array(v.synchronousWith.length)
				for (const item0 of v.synchronousWith) {
					w.str(item0)
				}
				n++
			}
			if (v.insecure !== undefined) {
				w.field(12)
				w.bool(v.insecure)
				n++
			}
			if (v.selfMetrics !== undefined) {
				w.field(13)
				w.bool(v.selfMetrics)
				n++
			}
			if (v.withoutSystem !== undefined) {
				w.field(14)
				w.bool(v.withoutSystem)
				n++
			}
			if (v.maxSeries !== undefined) {
				w.field(15)
				w.uint(v.maxSeries)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $durability: string | undefined
			let $encryptionKeyFile: string | undefined
			let $node: boolean | undefined
			let $from: string[] | undefined
			let $serve: string | undefined
			let $files: string[] | undefined
			let $token: string | undefined
			let $synchronous: boolean | undefined
			let $catalog: boolean | undefined
			let $advertise: string | undefined
			let $synchronousWith: string[] | undefined
			let $insecure: boolean | undefined
			let $selfMetrics: boolean | undefined
			let $withoutSystem: boolean | undefined
			let $maxSeries: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$durability = r.str()
						break
					case 2:
						$encryptionKeyFile = r.str()
						break
					case 3:
						$node = r.bool()
						break
					case 4: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$from = items0
						break
					}
					case 5:
						$serve = r.str()
						break
					case 6: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$files = items0
						break
					}
					case 7:
						$token = r.str()
						break
					case 8:
						$synchronous = r.bool()
						break
					case 9:
						$catalog = r.bool()
						break
					case 10:
						$advertise = r.str()
						break
					case 11: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$synchronousWith = items0
						break
					}
					case 12:
						$insecure = r.bool()
						break
					case 13:
						$selfMetrics = r.bool()
						break
					case 14:
						$withoutSystem = r.bool()
						break
					case 15:
						$maxSeries = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				durability: $durability,
				encryptionKeyFile: $encryptionKeyFile,
				node: $node,
				from: $from,
				serve: $serve,
				files: $files,
				token: $token,
				synchronous: $synchronous,
				catalog: $catalog,
				advertise: $advertise,
				synchronousWith: $synchronousWith,
				insecure: $insecure,
				selfMetrics: $selfMetrics,
				withoutSystem: $withoutSystem,
				maxSeries: $maxSeries,
			}
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
 * A key of a lock or a semaphore that a holder through a connection asks
 * for, as docs/guides/locks/README.md has it: the lease lasts `ttl` from its grant and
 * from each renewal, so that a holder whose program froze loses it.
 */
export const LocksHold = message(
	'locks.Hold',
	{
		/** lock or semaphore */
		kind: [1, str],
		/** [a-z0-9][a-z0-9_-]{0,63} */
		name: [2, str],
		/** 1 to 1024 bytes */
		key: [3, str],
		/** how many may hold the key at once: 1 for a lock, 1 to 1000 */
		limit: [4, uint],
		/** 1 s to 1 hour */
		ttl: [5, uint],
		/** from the grant, however the lease is renewed */
		maxHold: [6, uint],
		/** 0 takes the key only if it is free */
		wait: [7, uint],
		/** 16 random bytes: asked again, the lease given answers */
		request: [8, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.kind !== undefined) {
				w.field(1)
				w.str(v.kind)
				n++
			}
			if (v.name !== undefined) {
				w.field(2)
				w.str(v.name)
				n++
			}
			if (v.key !== undefined) {
				w.field(3)
				w.str(v.key)
				n++
			}
			if (v.limit !== undefined) {
				w.field(4)
				w.uint(v.limit)
				n++
			}
			if (v.ttl !== undefined) {
				w.field(5)
				w.uint(v.ttl)
				n++
			}
			if (v.maxHold !== undefined) {
				w.field(6)
				w.uint(v.maxHold)
				n++
			}
			if (v.wait !== undefined) {
				w.field(7)
				w.uint(v.wait)
				n++
			}
			if (v.request !== undefined) {
				w.field(8)
				w.bin(v.request)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $kind: string | undefined
			let $name: string | undefined
			let $key: string | undefined
			let $limit: number | undefined
			let $ttl: number | undefined
			let $maxHold: number | undefined
			let $wait: number | undefined
			let $request: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$kind = r.str()
						break
					case 2:
						$name = r.str()
						break
					case 3:
						$key = r.str()
						break
					case 4:
						$limit = r.uint()
						break
					case 5:
						$ttl = r.uint()
						break
					case 6:
						$maxHold = r.uint()
						break
					case 7:
						$wait = r.uint()
						break
					case 8:
						$request = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				kind: $kind,
				name: $name,
				key: $key,
				limit: $limit,
				ttl: $ttl,
				maxHold: $maxHold,
				wait: $wait,
				request: $request,
			}
		},
	},
)

/**
 * A lease, or the key held by others when the hold would not wait.
 */
export const LocksHeld = message(
	'locks.Held',
	{
		acquired: [1, bool],
		/** 16 random bytes only its holder learns */
		lease: [2, bin],
		/** a decimal integer of up to 128 bits; empty for a semaphore */
		fence: [3, str],
		/** how long until maxHold revokes the lease, from now */
		revokeIn: [4, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.acquired !== undefined) {
				w.field(1)
				w.bool(v.acquired)
				n++
			}
			if (v.lease !== undefined) {
				w.field(2)
				w.bin(v.lease)
				n++
			}
			if (v.fence !== undefined) {
				w.field(3)
				w.str(v.fence)
				n++
			}
			if (v.revokeIn !== undefined) {
				w.field(4)
				w.uint(v.revokeIn)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $acquired: boolean | undefined
			let $lease: Uint8Array | undefined
			let $fence: string | undefined
			let $revokeIn: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$acquired = r.bool()
						break
					case 2:
						$lease = r.bytes()
						break
					case 3:
						$fence = r.str()
						break
					case 4:
						$revokeIn = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { acquired: $acquired, lease: $lease, fence: $fence, revokeIn: $revokeIn }
		},
	},
)

export const LocksRenew = message(
	'locks.Renew',
	{
		lease: [1, bin],
		/** how long the lease lasts from now */
		ttl: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.lease !== undefined) {
				w.field(1)
				w.bin(v.lease)
				n++
			}
			if (v.ttl !== undefined) {
				w.field(2)
				w.uint(v.ttl)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $lease: Uint8Array | undefined
			let $ttl: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$lease = r.bytes()
						break
					case 2:
						$ttl = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { lease: $lease, ttl: $ttl }
		},
	},
)

export const LocksLease = message(
	'locks.Lease',
	{
		lease: [1, bin],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.lease !== undefined) {
				w.field(1)
				w.bin(v.lease)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $lease: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$lease = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { lease: $lease }
		},
	},
)

/**
 * What a label's value must be: one of values, none of them with not, the
 * label absent too, or a value that starts with prefix.
 */
export const MetricsCondition = message(
	'metrics.Condition',
	{
		values: [1, list(str)],
		not: [2, bool],
		prefix: [3, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.values !== undefined) {
				w.field(1)
				w.array(v.values.length)
				for (const item0 of v.values) {
					w.str(item0)
				}
				n++
			}
			if (v.not !== undefined) {
				w.field(2)
				w.bool(v.not)
				n++
			}
			if (v.prefix !== undefined) {
				w.field(3)
				w.str(v.prefix)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $values: string[] | undefined
			let $not: boolean | undefined
			let $prefix: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$values = items0
						break
					}
					case 2:
						$not = r.bool()
						break
					case 3:
						$prefix = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { values: $values, not: $not, prefix: $prefix }
		},
	},
)

/**
 * The series a call finds, and its range: since before now, or from and to.
 */
export const MetricsQuery = message(
	'metrics.Query',
	{
		name: [1, str],
		where: [2, names(MetricsCondition)],
		since: [3, uint],
		from: [4, int],
		/** excluded; now, a sample taken now in the range, unless given */
		to: [5, int],
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
			if (v.where !== undefined) {
				w.field(2)
				writeNames(w, v.where, MetricsCondition.write)
				n++
			}
			if (v.since !== undefined) {
				w.field(3)
				w.uint(v.since)
				n++
			}
			if (v.from !== undefined) {
				w.field(4)
				w.int(v.from)
				n++
			}
			if (v.to !== undefined) {
				w.field(5)
				w.int(v.to)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $where: Record<string, Read<typeof MetricsCondition.fields>> | undefined
			let $since: number | undefined
			let $from: number | undefined
			let $to: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$where = readNames(r, MetricsCondition.read)
						break
					case 3:
						$since = r.uint()
						break
					case 4:
						$from = r.int()
						break
					case 5:
						$to = r.int()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, where: $where, since: $since, from: $from, to: $to }
		},
	},
)

/**
 * A series and its samples, or a part of them: a series longer than a body
 * goes on in the next part.
 */
export const MetricsSeries = message(
	'metrics.Series',
	{
		name: [1, str],
		/** gauge, counter or histogram */
		kind: [2, str],
		labels: [3, names(str)],
		/** an int64 a sample */
		times: [4, bin],
		/** a float64 a sample */
		values: [5, bin],
		/** the next part goes on with this series */
		more: [6, bool],
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
			if (v.kind !== undefined) {
				w.field(2)
				w.str(v.kind)
				n++
			}
			if (v.labels !== undefined) {
				w.field(3)
				writeNames(w, v.labels, str.write)
				n++
			}
			if (v.times !== undefined) {
				w.field(4)
				w.bin(v.times)
				n++
			}
			if (v.values !== undefined) {
				w.field(5)
				w.bin(v.values)
				n++
			}
			if (v.more !== undefined) {
				w.field(6)
				w.bool(v.more)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $kind: string | undefined
			let $labels: Record<string, string> | undefined
			let $times: Uint8Array | undefined
			let $values: Uint8Array | undefined
			let $more: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$kind = r.str()
						break
					case 3:
						$labels = readNames(r, str.read)
						break
					case 4:
						$times = r.bytes()
						break
					case 5:
						$values = r.bytes()
						break
					case 6:
						$more = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				name: $name,
				kind: $kind,
				labels: $labels,
				times: $times,
				values: $values,
				more: $more,
			}
		},
	},
)

export const MetricsRead = message(
	'metrics.Read',
	{
		series: [1, list(MetricsSeries)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.series !== undefined) {
				w.field(1)
				w.array(v.series.length)
				for (const item0 of v.series) {
					MetricsSeries.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $series: Read<typeof MetricsSeries.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof MetricsSeries.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsSeries.read(r))
						}
						r.leave()
						$series = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { series: $series }
		},
	},
)

/**
 * A series' newest sample in the range.
 */
export const MetricsNewest = message(
	'metrics.Newest',
	{
		name: [1, str],
		labels: [2, names(str)],
		at: [3, int],
		value: [4, float],
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
			if (v.labels !== undefined) {
				w.field(2)
				writeNames(w, v.labels, str.write)
				n++
			}
			if (v.at !== undefined) {
				w.field(3)
				w.int(v.at)
				n++
			}
			if (v.value !== undefined) {
				w.field(4)
				w.float(v.value)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $labels: Record<string, string> | undefined
			let $at: number | undefined
			let $value: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$labels = readNames(r, str.read)
						break
					case 3:
						$at = r.int()
						break
					case 4:
						$value = r.float()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, labels: $labels, at: $at, value: $value }
		},
	},
)

export const MetricsLatest = message(
	'metrics.Latest',
	{
		series: [1, list(MetricsNewest)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.series !== undefined) {
				w.field(1)
				w.array(v.series.length)
				for (const item0 of v.series) {
					MetricsNewest.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $series: Read<typeof MetricsNewest.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof MetricsNewest.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsNewest.read(r))
						}
						r.leave()
						$series = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { series: $series }
		},
	},
)

/**
 * An aggregate: read's query, its buckets, its op and its groups. Without
 * by and without, every series of a name is one group; without: [] keeps
 * every series apart.
 */
export const MetricsAggregateAsked = message(
	'metrics.AggregateAsked',
	{
		query: [1, MetricsQuery],
		every: [2, uint],
		/** count, sum, avg, min, max, increase, rate, delta, first, last, or p and a rank; none for a read's plan */
		op: [3, str],
		by: [4, list(str)],
		without: [5, list(str)],
		/** a timer's percentile also gives a point between its bounds */
		estimate: [6, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.query !== undefined) {
				w.field(1)
				MetricsQuery.write(w, v.query)
				n++
			}
			if (v.every !== undefined) {
				w.field(2)
				w.uint(v.every)
				n++
			}
			if (v.op !== undefined) {
				w.field(3)
				w.str(v.op)
				n++
			}
			if (v.by !== undefined) {
				w.field(4)
				w.array(v.by.length)
				for (const item0 of v.by) {
					w.str(item0)
				}
				n++
			}
			if (v.without !== undefined) {
				w.field(5)
				w.array(v.without.length)
				for (const item0 of v.without) {
					w.str(item0)
				}
				n++
			}
			if (v.estimate !== undefined) {
				w.field(6)
				w.bool(v.estimate)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $query: Read<typeof MetricsQuery.fields> | undefined
			let $every: number | undefined
			let $op: string | undefined
			let $by: string[] | undefined
			let $without: string[] | undefined
			let $estimate: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$query = MetricsQuery.read(r)
						break
					case 2:
						$every = r.uint()
						break
					case 3:
						$op = r.str()
						break
					case 4: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$by = items0
						break
					}
					case 5: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$without = items0
						break
					}
					case 6:
						$estimate = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				query: $query,
				every: $every,
				op: $op,
				by: $by,
				without: $without,
				estimate: $estimate,
			}
		},
	},
)

/**
 * A bucket of an aggregate: its value, or a timer's percentile's bounds.
 */
export const MetricsBucket = message(
	'metrics.Bucket',
	{
		/** where it begins */
		at: [1, int],
		/** none for a timer's percentile */
		value: [2, float],
		lower: [3, float],
		/** infinity past the last bound */
		upper: [4, float],
		estimate: [5, float],
		samples: [6, uint],
		resets: [7, uint],
		/** its value comes from less than its span */
		partial: [8, bool],
		/** its first step began before the bucket before it */
		gap: [9, bool],
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
			if (v.value !== undefined) {
				w.field(2)
				w.float(v.value)
				n++
			}
			if (v.lower !== undefined) {
				w.field(3)
				w.float(v.lower)
				n++
			}
			if (v.upper !== undefined) {
				w.field(4)
				w.float(v.upper)
				n++
			}
			if (v.estimate !== undefined) {
				w.field(5)
				w.float(v.estimate)
				n++
			}
			if (v.samples !== undefined) {
				w.field(6)
				w.uint(v.samples)
				n++
			}
			if (v.resets !== undefined) {
				w.field(7)
				w.uint(v.resets)
				n++
			}
			if (v.partial !== undefined) {
				w.field(8)
				w.bool(v.partial)
				n++
			}
			if (v.gap !== undefined) {
				w.field(9)
				w.bool(v.gap)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $at: number | undefined
			let $value: number | undefined
			let $lower: number | undefined
			let $upper: number | undefined
			let $estimate: number | undefined
			let $samples: number | undefined
			let $resets: number | undefined
			let $partial: boolean | undefined
			let $gap: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$at = r.int()
						break
					case 2:
						$value = r.float()
						break
					case 3:
						$lower = r.float()
						break
					case 4:
						$upper = r.float()
						break
					case 5:
						$estimate = r.float()
						break
					case 6:
						$samples = r.uint()
						break
					case 7:
						$resets = r.uint()
						break
					case 8:
						$partial = r.bool()
						break
					case 9:
						$gap = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				at: $at,
				value: $value,
				lower: $lower,
				upper: $upper,
				estimate: $estimate,
				samples: $samples,
				resets: $resets,
				partial: $partial,
				gap: $gap,
			}
		},
	},
)

/**
 * A group's buckets, or a part of them: a group longer than a body goes on
 * in the next part.
 */
export const MetricsGroup = message(
	'metrics.Group',
	{
		name: [1, str],
		labels: [2, names(str)],
		buckets: [3, list(MetricsBucket)],
		more: [4, bool],
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
			if (v.labels !== undefined) {
				w.field(2)
				writeNames(w, v.labels, str.write)
				n++
			}
			if (v.buckets !== undefined) {
				w.field(3)
				w.array(v.buckets.length)
				for (const item0 of v.buckets) {
					MetricsBucket.write(w, item0)
				}
				n++
			}
			if (v.more !== undefined) {
				w.field(4)
				w.bool(v.more)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $labels: Record<string, string> | undefined
			let $buckets: Read<typeof MetricsBucket.fields>[] | undefined
			let $more: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$labels = readNames(r, str.read)
						break
					case 3: {
						const items0: Read<typeof MetricsBucket.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsBucket.read(r))
						}
						r.leave()
						$buckets = items0
						break
					}
					case 4:
						$more = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, labels: $labels, buckets: $buckets, more: $more }
		},
	},
)

export const MetricsAggregates = message(
	'metrics.Aggregates',
	{
		groups: [1, list(MetricsGroup)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.groups !== undefined) {
				w.field(1)
				w.array(v.groups.length)
				for (const item0 of v.groups) {
					MetricsGroup.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $groups: Read<typeof MetricsGroup.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof MetricsGroup.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsGroup.read(r))
						}
						r.leave()
						$groups = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { groups: $groups }
		},
	},
)

export const MetricsExceeds = message(
	'metrics.Exceeds',
	{
		/** series, bytes read or samples decoded */
		limit: [1, str],
		wanted: [2, uint],
		bound: [3, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.limit !== undefined) {
				w.field(1)
				w.str(v.limit)
				n++
			}
			if (v.wanted !== undefined) {
				w.field(2)
				w.uint(v.wanted)
				n++
			}
			if (v.bound !== undefined) {
				w.field(3)
				w.uint(v.bound)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $limit: string | undefined
			let $wanted: number | undefined
			let $bound: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$limit = r.str()
						break
					case 2:
						$wanted = r.uint()
						break
					case 3:
						$bound = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { limit: $limit, wanted: $wanted, bound: $bound }
		},
	},
)

/**
 * A series to write, its samples as two columns.
 */
export const MetricsNewSeries = message(
	'metrics.NewSeries',
	{
		name: [1, str],
		/** gauge, counter or histogram */
		kind: [2, str],
		labels: [3, names(str)],
		/** an int64 a sample */
		times: [4, bin],
		/** a float64 a sample */
		values: [5, bin],
		/** how long the name's samples stay: its own, kept in the file */
		keep: [6, uint],
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
			if (v.kind !== undefined) {
				w.field(2)
				w.str(v.kind)
				n++
			}
			if (v.labels !== undefined) {
				w.field(3)
				writeNames(w, v.labels, str.write)
				n++
			}
			if (v.times !== undefined) {
				w.field(4)
				w.bin(v.times)
				n++
			}
			if (v.values !== undefined) {
				w.field(5)
				w.bin(v.values)
				n++
			}
			if (v.keep !== undefined) {
				w.field(6)
				w.uint(v.keep)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $kind: string | undefined
			let $labels: Record<string, string> | undefined
			let $times: Uint8Array | undefined
			let $values: Uint8Array | undefined
			let $keep: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$kind = r.str()
						break
					case 3:
						$labels = readNames(r, str.read)
						break
					case 4:
						$times = r.bytes()
						break
					case 5:
						$values = r.bytes()
						break
					case 6:
						$keep = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				name: $name,
				kind: $kind,
				labels: $labels,
				times: $times,
				values: $values,
				keep: $keep,
			}
		},
	},
)

/**
 * Series to write, every one or none.
 */
export const MetricsIngest = message(
	'metrics.Ingest',
	{
		series: [1, list(MetricsNewSeries)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.series !== undefined) {
				w.field(1)
				w.array(v.series.length)
				for (const item0 of v.series) {
					MetricsNewSeries.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $series: Read<typeof MetricsNewSeries.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof MetricsNewSeries.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsNewSeries.read(r))
						}
						r.leave()
						$series = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { series: $series }
		},
	},
)

/**
 * A text an exporter serves, in Prometheus' format or OpenMetrics', whose
 * series the store writes every one or none, each with labels beside its own.
 */
export const MetricsExposition = message(
	'metrics.Exposition',
	{
		/** UTF-8, read a line at a time, so that an error names its line */
		text: [1, bin],
		labels: [2, names(str)],
		/** of a sample the text gives no time; now unless given */
		at: [3, int],
		/** how long each name of the text keeps its samples */
		keep: [4, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.text !== undefined) {
				w.field(1)
				w.bin(v.text)
				n++
			}
			if (v.labels !== undefined) {
				w.field(2)
				writeNames(w, v.labels, str.write)
				n++
			}
			if (v.at !== undefined) {
				w.field(3)
				w.int(v.at)
				n++
			}
			if (v.keep !== undefined) {
				w.field(4)
				w.uint(v.keep)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $text: Uint8Array | undefined
			let $labels: Record<string, string> | undefined
			let $at: number | undefined
			let $keep: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$text = r.bytes()
						break
					case 2:
						$labels = readNames(r, str.read)
						break
					case 3:
						$at = r.int()
						break
					case 4:
						$keep = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { text: $text, labels: $labels, at: $at, keep: $keep }
		},
	},
)

/**
 * A page of names, of a name's labels or of a label's values.
 */
export const MetricsPageAsked = message(
	'metrics.PageAsked',
	{
		/** the name whose labels, or whose label's values, it lists */
		name: [1, str],
		/** the label whose values it lists */
		label: [2, str],
		prefix: [3, str],
		/** a page's next */
		after: [4, str],
		/** 1000 unless given, 10,000 at most */
		limit: [5, uint],
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
			if (v.label !== undefined) {
				w.field(2)
				w.str(v.label)
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
			let $name: string | undefined
			let $label: string | undefined
			let $prefix: string | undefined
			let $after: string | undefined
			let $limit: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$label = r.str()
						break
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
			return { name: $name, label: $label, prefix: $prefix, after: $after, limit: $limit }
		},
	},
)

/**
 * A name the store holds samples of.
 */
export const MetricsName = message(
	'metrics.Name',
	{
		name: [1, str],
		kind: [2, str],
		seriesCount: [3, uint],
		unit: [4, str],
		help: [5, str],
		keep: [6, uint],
		/** its oldest sample, or where its keep begins while it holds older */
		retainedFrom: [7, int],
		newest: [8, int],
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
			if (v.kind !== undefined) {
				w.field(2)
				w.str(v.kind)
				n++
			}
			if (v.seriesCount !== undefined) {
				w.field(3)
				w.uint(v.seriesCount)
				n++
			}
			if (v.unit !== undefined) {
				w.field(4)
				w.str(v.unit)
				n++
			}
			if (v.help !== undefined) {
				w.field(5)
				w.str(v.help)
				n++
			}
			if (v.keep !== undefined) {
				w.field(6)
				w.uint(v.keep)
				n++
			}
			if (v.retainedFrom !== undefined) {
				w.field(7)
				w.int(v.retainedFrom)
				n++
			}
			if (v.newest !== undefined) {
				w.field(8)
				w.int(v.newest)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $kind: string | undefined
			let $seriesCount: number | undefined
			let $unit: string | undefined
			let $help: string | undefined
			let $keep: number | undefined
			let $retainedFrom: number | undefined
			let $newest: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$kind = r.str()
						break
					case 3:
						$seriesCount = r.uint()
						break
					case 4:
						$unit = r.str()
						break
					case 5:
						$help = r.str()
						break
					case 6:
						$keep = r.uint()
						break
					case 7:
						$retainedFrom = r.int()
						break
					case 8:
						$newest = r.int()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				name: $name,
				kind: $kind,
				seriesCount: $seriesCount,
				unit: $unit,
				help: $help,
				keep: $keep,
				retainedFrom: $retainedFrom,
				newest: $newest,
			}
		},
	},
)

export const MetricsNamePage = message(
	'metrics.NamePage',
	{
		names: [1, list(MetricsName)],
		/** the page after it; the last page has none */
		next: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.names !== undefined) {
				w.field(1)
				w.array(v.names.length)
				for (const item0 of v.names) {
					MetricsName.write(w, item0)
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
			let $names: Read<typeof MetricsName.fields>[] | undefined
			let $next: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof MetricsName.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsName.read(r))
						}
						r.leave()
						$names = items0
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
			return { names: $names, next: $next }
		},
	},
)

/**
 * A page of a name's labels, or of a label's values.
 */
export const MetricsTextPage = message(
	'metrics.TextPage',
	{
		texts: [1, list(str)],
		next: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.texts !== undefined) {
				w.field(1)
				w.array(v.texts.length)
				for (const item0 of v.texts) {
					w.str(item0)
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
			let $texts: string[] | undefined
			let $next: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$texts = items0
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
			return { texts: $texts, next: $next }
		},
	},
)

/**
 * A name's unit and help; empty ones remove them.
 */
export const MetricsDescription = message(
	'metrics.Description',
	{
		name: [1, str],
		unit: [2, str],
		help: [3, str],
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
			if (v.unit !== undefined) {
				w.field(2)
				w.str(v.unit)
				n++
			}
			if (v.help !== undefined) {
				w.field(3)
				w.str(v.help)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $unit: string | undefined
			let $help: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$unit = r.str()
						break
					case 3:
						$help = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, unit: $unit, help: $help }
		},
	},
)

export const MetricsNameAsked = message(
	'metrics.NameAsked',
	{
		name: [1, str],
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
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name }
		},
	},
)

/**
 * One series, by exactly its name and labels.
 */
export const MetricsSeriesNamed = message(
	'metrics.SeriesNamed',
	{
		name: [1, str],
		labels: [2, names(str)],
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
			if (v.labels !== undefined) {
				w.field(2)
				writeNames(w, v.labels, str.write)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $labels: Record<string, string> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$labels = readNames(r, str.read)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, labels: $labels }
		},
	},
)

export const MetricsDeleted = message(
	'metrics.Deleted',
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
 * A series whose bytes no longer read, and what broke.
 */
export const MetricsDamage = message(
	'metrics.Damage',
	{
		name: [1, str],
		labels: [2, names(str)],
		error: [3, str],
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
			if (v.labels !== undefined) {
				w.field(2)
				writeNames(w, v.labels, str.write)
				n++
			}
			if (v.error !== undefined) {
				w.field(3)
				w.str(v.error)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $labels: Record<string, string> | undefined
			let $error: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$labels = readNames(r, str.read)
						break
					case 3:
						$error = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, labels: $labels, error: $error }
		},
	},
)

export const MetricsDamages = message(
	'metrics.Damages',
	{
		damages: [1, list(MetricsDamage)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.damages !== undefined) {
				w.field(1)
				w.array(v.damages.length)
				for (const item0 of v.damages) {
					MetricsDamage.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $damages: Read<typeof MetricsDamage.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof MetricsDamage.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsDamage.read(r))
						}
						r.leave()
						$damages = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { damages: $damages }
		},
	},
)

/**
 * An SDK's instruments' stream: its session, sixteen random bytes drawn when
 * it opened the store, and the last batch the store answered it, 0 for none.
 */
export const MetricsSession = message(
	'metrics.Session',
	{
		id: [1, bin],
		after: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.id !== undefined) {
				w.field(1)
				w.bin(v.id)
				n++
			}
			if (v.after !== undefined) {
				w.field(2)
				w.uint(v.after)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $id: Uint8Array | undefined
			let $after: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$id = r.bytes()
						break
					case 2:
						$after = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { id: $id, after: $after }
		},
	},
)

/**
 * An instrument a batch counts into, by a number the stream gives it.
 */
export const MetricsInstrument = message(
	'metrics.Instrument',
	{
		number: [1, uint],
		name: [2, str],
		/** counter, gauge, timer or histogram */
		form: [3, str],
		unit: [4, str],
		help: [5, str],
		keep: [6, uint],
		/** a timer's or a histogram's upper bounds, ascending */
		bounds: [7, list(float)],
		/** a timer's spread on the store's grid */
		exponential: [8, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.number !== undefined) {
				w.field(1)
				w.uint(v.number)
				n++
			}
			if (v.name !== undefined) {
				w.field(2)
				w.str(v.name)
				n++
			}
			if (v.form !== undefined) {
				w.field(3)
				w.str(v.form)
				n++
			}
			if (v.unit !== undefined) {
				w.field(4)
				w.str(v.unit)
				n++
			}
			if (v.help !== undefined) {
				w.field(5)
				w.str(v.help)
				n++
			}
			if (v.keep !== undefined) {
				w.field(6)
				w.uint(v.keep)
				n++
			}
			if (v.bounds !== undefined) {
				w.field(7)
				w.array(v.bounds.length)
				for (const item0 of v.bounds) {
					w.float(item0)
				}
				n++
			}
			if (v.exponential !== undefined) {
				w.field(8)
				w.bool(v.exponential)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $number: number | undefined
			let $name: string | undefined
			let $form: string | undefined
			let $unit: string | undefined
			let $help: string | undefined
			let $keep: number | undefined
			let $bounds: number[] | undefined
			let $exponential: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$number = r.uint()
						break
					case 2:
						$name = r.str()
						break
					case 3:
						$form = r.str()
						break
					case 4:
						$unit = r.str()
						break
					case 5:
						$help = r.str()
						break
					case 6:
						$keep = r.uint()
						break
					case 7: {
						const items0: number[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.float())
						}
						r.leave()
						$bounds = items0
						break
					}
					case 8:
						$exponential = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				number: $number,
				name: $name,
				form: $form,
				unit: $unit,
				help: $help,
				keep: $keep,
				bounds: $bounds,
				exponential: $exponential,
			}
		},
	},
)

/**
 * A set of labels of an instrument, by a number the stream gives it, or its
 * series of __overflow, where a client counts past the sets it holds.
 */
export const MetricsSet = message(
	'metrics.Set',
	{
		number: [1, uint],
		instrument: [2, uint],
		labels: [3, names(str)],
		overflow: [4, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.number !== undefined) {
				w.field(1)
				w.uint(v.number)
				n++
			}
			if (v.instrument !== undefined) {
				w.field(2)
				w.uint(v.instrument)
				n++
			}
			if (v.labels !== undefined) {
				w.field(3)
				writeNames(w, v.labels, str.write)
				n++
			}
			if (v.overflow !== undefined) {
				w.field(4)
				w.bool(v.overflow)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $number: number | undefined
			let $instrument: number | undefined
			let $labels: Record<string, string> | undefined
			let $overflow: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$number = r.uint()
						break
					case 2:
						$instrument = r.uint()
						break
					case 3:
						$labels = readNames(r, str.read)
						break
					case 4:
						$overflow = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { number: $number, instrument: $instrument, labels: $labels, overflow: $overflow }
		},
	},
)

/**
 * What a set of labels changed since the batch before: a counter's sum to
 * add, a gauge's value, or what a timer or a histogram measured.
 */
export const MetricsChange = message(
	'metrics.Change',
	{
		set: [1, uint],
		add: [2, float],
		value: [3, float],
		count: [4, uint],
		sum: [5, float],
		longest: [6, float],
		/** each bound's own count, +Inf last */
		buckets: [7, list(uint)],
		/** each interval of the grid's count, by its label: 3:13, zero */
		bins: [8, names(uint)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.set !== undefined) {
				w.field(1)
				w.uint(v.set)
				n++
			}
			if (v.add !== undefined) {
				w.field(2)
				w.float(v.add)
				n++
			}
			if (v.value !== undefined) {
				w.field(3)
				w.float(v.value)
				n++
			}
			if (v.count !== undefined) {
				w.field(4)
				w.uint(v.count)
				n++
			}
			if (v.sum !== undefined) {
				w.field(5)
				w.float(v.sum)
				n++
			}
			if (v.longest !== undefined) {
				w.field(6)
				w.float(v.longest)
				n++
			}
			if (v.buckets !== undefined) {
				w.field(7)
				w.array(v.buckets.length)
				for (const item0 of v.buckets) {
					w.uint(item0)
				}
				n++
			}
			if (v.bins !== undefined) {
				w.field(8)
				writeNames(w, v.bins, uint.write)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $set: number | undefined
			let $add: number | undefined
			let $value: number | undefined
			let $count: number | undefined
			let $sum: number | undefined
			let $longest: number | undefined
			let $buckets: number[] | undefined
			let $bins: Record<string, number> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$set = r.uint()
						break
					case 2:
						$add = r.float()
						break
					case 3:
						$value = r.float()
						break
					case 4:
						$count = r.uint()
						break
					case 5:
						$sum = r.float()
						break
					case 6:
						$longest = r.float()
						break
					case 7: {
						const items0: number[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.uint())
						}
						r.leave()
						$buckets = items0
						break
					}
					case 8:
						$bins = readNames(r, uint.read)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				set: $set,
				add: $add,
				value: $value,
				count: $count,
				sum: $sum,
				longest: $longest,
				buckets: $buckets,
				bins: $bins,
			}
		},
	},
)

/**
 * A batch of an SDK's changes, numbered from 1 in its session, with the
 * instruments and the sets of labels it names that the stream has not
 * declared yet.
 */
export const MetricsBatch = message(
	'metrics.Batch',
	{
		number: [1, uint],
		instruments: [2, list(MetricsInstrument)],
		sets: [3, list(MetricsSet)],
		changes: [4, list(MetricsChange)],
		/** the store writes its totals before it answers */
		flush: [5, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.number !== undefined) {
				w.field(1)
				w.uint(v.number)
				n++
			}
			if (v.instruments !== undefined) {
				w.field(2)
				w.array(v.instruments.length)
				for (const item0 of v.instruments) {
					MetricsInstrument.write(w, item0)
				}
				n++
			}
			if (v.sets !== undefined) {
				w.field(3)
				w.array(v.sets.length)
				for (const item0 of v.sets) {
					MetricsSet.write(w, item0)
				}
				n++
			}
			if (v.changes !== undefined) {
				w.field(4)
				w.array(v.changes.length)
				for (const item0 of v.changes) {
					MetricsChange.write(w, item0)
				}
				n++
			}
			if (v.flush !== undefined) {
				w.field(5)
				w.bool(v.flush)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $number: number | undefined
			let $instruments: Read<typeof MetricsInstrument.fields>[] | undefined
			let $sets: Read<typeof MetricsSet.fields>[] | undefined
			let $changes: Read<typeof MetricsChange.fields>[] | undefined
			let $flush: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$number = r.uint()
						break
					case 2: {
						const items0: Read<typeof MetricsInstrument.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsInstrument.read(r))
						}
						r.leave()
						$instruments = items0
						break
					}
					case 3: {
						const items0: Read<typeof MetricsSet.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsSet.read(r))
						}
						r.leave()
						$sets = items0
						break
					}
					case 4: {
						const items0: Read<typeof MetricsChange.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsChange.read(r))
						}
						r.leave()
						$changes = items0
						break
					}
					case 5:
						$flush = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				number: $number,
				instruments: $instruments,
				sets: $sets,
				changes: $changes,
				flush: $flush,
			}
		},
	},
)

/**
 * An instrument the store refused, by the stream's number for it.
 */
export const MetricsRefused = message(
	'metrics.Refused',
	{
		instrument: [1, uint],
		error: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.instrument !== undefined) {
				w.field(1)
				w.uint(v.instrument)
				n++
			}
			if (v.error !== undefined) {
				w.field(2)
				w.str(v.error)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $instrument: number | undefined
			let $error: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$instrument = r.uint()
						break
					case 2:
						$error = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { instrument: $instrument, error: $error }
		},
	},
)

/**
 * The store's answer: the last batch of the session it applied, and whether
 * it knew the session; the stream's RESPONSE says it first.
 */
export const MetricsAck = message(
	'metrics.Ack',
	{
		applied: [1, uint],
		known: [2, bool],
		refused: [3, list(MetricsRefused)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.applied !== undefined) {
				w.field(1)
				w.uint(v.applied)
				n++
			}
			if (v.known !== undefined) {
				w.field(2)
				w.bool(v.known)
				n++
			}
			if (v.refused !== undefined) {
				w.field(3)
				w.array(v.refused.length)
				for (const item0 of v.refused) {
					MetricsRefused.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $applied: number | undefined
			let $known: boolean | undefined
			let $refused: Read<typeof MetricsRefused.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$applied = r.uint()
						break
					case 2:
						$known = r.bool()
						break
					case 3: {
						const items0: Read<typeof MetricsRefused.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(MetricsRefused.read(r))
						}
						r.leave()
						$refused = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { applied: $applied, known: $known, refused: $refused }
		},
	},
)

/**
 * Opens a stream of events by name, made with its first event.
 */
export const RecordsOpen = message(
	'records.Open',
	{
		/** [a-z0-9][a-z0-9_-]{0,63} */
		stream: [1, str],
		/** how long it keeps its events: its own, kept in the file */
		keep: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.stream !== undefined) {
				w.field(1)
				w.str(v.stream)
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
			let $stream: string | undefined
			let $keep: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$stream = r.str()
						break
					case 2:
						$keep = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { stream: $stream, keep: $keep }
		},
	},
)

/**
 * An event to write.
 */
export const RecordsNewEvent = message(
	'records.NewEvent',
	{
		name: [1, str],
		/** the store's clock's now unless given */
		at: [2, int64],
		/** -4 debug, 0 info, 4 warn, 8 error, between them as slog counts */
		level: [3, int],
		message: [4, str],
		/** an object */
		fields: [5, str],
		/** an object */
		context: [6, str],
		/** 16 bytes */
		traceId: [7, bin],
		/** 8 bytes */
		spanId: [8, bin],
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
			if (v.at !== undefined) {
				w.field(2)
				w.int(v.at)
				n++
			}
			if (v.level !== undefined) {
				w.field(3)
				w.int(v.level)
				n++
			}
			if (v.message !== undefined) {
				w.field(4)
				w.str(v.message)
				n++
			}
			if (v.fields !== undefined) {
				w.field(5)
				w.str(v.fields)
				n++
			}
			if (v.context !== undefined) {
				w.field(6)
				w.str(v.context)
				n++
			}
			if (v.traceId !== undefined) {
				w.field(7)
				w.bin(v.traceId)
				n++
			}
			if (v.spanId !== undefined) {
				w.field(8)
				w.bin(v.spanId)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $at: bigint | undefined
			let $level: number | undefined
			let $message: string | undefined
			let $fields: string | undefined
			let $context: string | undefined
			let $traceId: Uint8Array | undefined
			let $spanId: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$at = r.int64()
						break
					case 3:
						$level = r.int()
						break
					case 4:
						$message = r.str()
						break
					case 5:
						$fields = r.str()
						break
					case 6:
						$context = r.str()
						break
					case 7:
						$traceId = r.bytes()
						break
					case 8:
						$spanId = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				name: $name,
				at: $at,
				level: $level,
				message: $message,
				fields: $fields,
				context: $context,
				traceId: $traceId,
				spanId: $spanId,
			}
		},
	},
)

/**
 * Events to write to a stream, all of them or none.
 */
export const RecordsAppend = message(
	'records.Append',
	{
		handle: [1, uint],
		events: [2, list(RecordsNewEvent)],
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
			if (v.events !== undefined) {
				w.field(2)
				w.array(v.events.length)
				for (const item0 of v.events) {
					RecordsNewEvent.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $events: Read<typeof RecordsNewEvent.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: Read<typeof RecordsNewEvent.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(RecordsNewEvent.read(r))
						}
						r.leave()
						$events = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, events: $events }
		},
	},
)

/**
 * An event as a read gives it.
 */
export const RecordsEvent = message(
	'records.Event',
	{
		at: [1, int64],
		stream: [2, str],
		name: [3, str],
		level: [4, int],
		message: [5, str],
		/** an object; none when it has no field */
		fields: [6, str],
		context: [7, str],
		traceId: [8, bin],
		spanId: [9, bin],
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
			if (v.stream !== undefined) {
				w.field(2)
				w.str(v.stream)
				n++
			}
			if (v.name !== undefined) {
				w.field(3)
				w.str(v.name)
				n++
			}
			if (v.level !== undefined) {
				w.field(4)
				w.int(v.level)
				n++
			}
			if (v.message !== undefined) {
				w.field(5)
				w.str(v.message)
				n++
			}
			if (v.fields !== undefined) {
				w.field(6)
				w.str(v.fields)
				n++
			}
			if (v.context !== undefined) {
				w.field(7)
				w.str(v.context)
				n++
			}
			if (v.traceId !== undefined) {
				w.field(8)
				w.bin(v.traceId)
				n++
			}
			if (v.spanId !== undefined) {
				w.field(9)
				w.bin(v.spanId)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $at: bigint | undefined
			let $stream: string | undefined
			let $name: string | undefined
			let $level: number | undefined
			let $message: string | undefined
			let $fields: string | undefined
			let $context: string | undefined
			let $traceId: Uint8Array | undefined
			let $spanId: Uint8Array | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$at = r.int64()
						break
					case 2:
						$stream = r.str()
						break
					case 3:
						$name = r.str()
						break
					case 4:
						$level = r.int()
						break
					case 5:
						$message = r.str()
						break
					case 6:
						$fields = r.str()
						break
					case 7:
						$context = r.str()
						break
					case 8:
						$traceId = r.bytes()
						break
					case 9:
						$spanId = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				at: $at,
				stream: $stream,
				name: $name,
				level: $level,
				message: $message,
				fields: $fields,
				context: $context,
				traceId: $traceId,
				spanId: $spanId,
			}
		},
	},
)

/**
 * What a read finds: each condition given holds.
 */
export const RecordsQuery = message(
	'records.Query',
	{
		/** the stream opened as it; every stream without one */
		handle: [1, uint],
		/** the span before now; or from and to */
		since: [2, uint],
		from: [3, int64],
		to: [4, int64],
		streams: [5, list(str)],
		names: [6, list(str)],
		minLevel: [7, int],
		traceId: [8, bin],
		/** each key with this value, in fields or in context */
		where: [9, names(str)],
		search: [10, str],
		newest: [11, bool],
		/** a page's events, top's values, fieldNames' keys */
		limit: [12, uint],
		/** a page's next */
		after: [13, str],
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
			if (v.since !== undefined) {
				w.field(2)
				w.uint(v.since)
				n++
			}
			if (v.from !== undefined) {
				w.field(3)
				w.int(v.from)
				n++
			}
			if (v.to !== undefined) {
				w.field(4)
				w.int(v.to)
				n++
			}
			if (v.streams !== undefined) {
				w.field(5)
				w.array(v.streams.length)
				for (const item0 of v.streams) {
					w.str(item0)
				}
				n++
			}
			if (v.names !== undefined) {
				w.field(6)
				w.array(v.names.length)
				for (const item0 of v.names) {
					w.str(item0)
				}
				n++
			}
			if (v.minLevel !== undefined) {
				w.field(7)
				w.int(v.minLevel)
				n++
			}
			if (v.traceId !== undefined) {
				w.field(8)
				w.bin(v.traceId)
				n++
			}
			if (v.where !== undefined) {
				w.field(9)
				writeNames(w, v.where, str.write)
				n++
			}
			if (v.search !== undefined) {
				w.field(10)
				w.str(v.search)
				n++
			}
			if (v.newest !== undefined) {
				w.field(11)
				w.bool(v.newest)
				n++
			}
			if (v.limit !== undefined) {
				w.field(12)
				w.uint(v.limit)
				n++
			}
			if (v.after !== undefined) {
				w.field(13)
				w.str(v.after)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $since: number | undefined
			let $from: bigint | undefined
			let $to: bigint | undefined
			let $streams: string[] | undefined
			let $names: string[] | undefined
			let $minLevel: number | undefined
			let $traceId: Uint8Array | undefined
			let $where: Record<string, string> | undefined
			let $search: string | undefined
			let $newest: boolean | undefined
			let $limit: number | undefined
			let $after: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$since = r.uint()
						break
					case 3:
						$from = r.int64()
						break
					case 4:
						$to = r.int64()
						break
					case 5: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$streams = items0
						break
					}
					case 6: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$names = items0
						break
					}
					case 7:
						$minLevel = r.int()
						break
					case 8:
						$traceId = r.bytes()
						break
					case 9:
						$where = readNames(r, str.read)
						break
					case 10:
						$search = r.str()
						break
					case 11:
						$newest = r.bool()
						break
					case 12:
						$limit = r.uint()
						break
					case 13:
						$after = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				handle: $handle,
				since: $since,
				from: $from,
				to: $to,
				streams: $streams,
				names: $names,
				minLevel: $minLevel,
				traceId: $traceId,
				where: $where,
				search: $search,
				newest: $newest,
				limit: $limit,
				after: $after,
			}
		},
	},
)

export const RecordsPage = message(
	'records.Page',
	{
		events: [1, list(RecordsEvent)],
		/** the page after it; the last page has none */
		next: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.events !== undefined) {
				w.field(1)
				w.array(v.events.length)
				for (const item0 of v.events) {
					RecordsEvent.write(w, item0)
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
			let $events: Read<typeof RecordsEvent.fields>[] | undefined
			let $next: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof RecordsEvent.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(RecordsEvent.read(r))
						}
						r.leave()
						$events = items0
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
			return { events: $events, next: $next }
		},
	},
)

export const RecordsCount = message(
	'records.Count',
	{
		count: [1, uint],
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
			w.closeMap(head, n)
		},
		read(r) {
			let $count: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$count = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { count: $count }
		},
	},
)

/**
 * A key's most frequent values among what a query finds.
 */
export const RecordsTopAsked = message(
	'records.TopAsked',
	{
		query: [1, RecordsQuery],
		/** a field's or a context's, or stream, name or level */
		key: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.query !== undefined) {
				w.field(1)
				RecordsQuery.write(w, v.query)
				n++
			}
			if (v.key !== undefined) {
				w.field(2)
				w.str(v.key)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $query: Read<typeof RecordsQuery.fields> | undefined
			let $key: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$query = RecordsQuery.read(r)
						break
					case 2:
						$key = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { query: $query, key: $key }
		},
	},
)

export const RecordsTop = message(
	'records.Top',
	{
		value: [1, str],
		count: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.value !== undefined) {
				w.field(1)
				w.str(v.value)
				n++
			}
			if (v.count !== undefined) {
				w.field(2)
				w.uint(v.count)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $value: string | undefined
			let $count: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$value = r.str()
						break
					case 2:
						$count = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { value: $value, count: $count }
		},
	},
)

export const RecordsTops = message(
	'records.Tops',
	{
		values: [1, list(RecordsTop)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.values !== undefined) {
				w.field(1)
				w.array(v.values.length)
				for (const item0 of v.values) {
					RecordsTop.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $values: Read<typeof RecordsTop.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof RecordsTop.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(RecordsTop.read(r))
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
			return { values: $values }
		},
	},
)

/**
 * A row for each span of every and each value of the keys of by.
 */
export const RecordsAggregateAsked = message(
	'records.AggregateAsked',
	{
		query: [1, RecordsQuery],
		every: [2, uint],
		by: [3, list(str)],
		stats: [4, list(str)],
		distinct: [5, list(str)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.query !== undefined) {
				w.field(1)
				RecordsQuery.write(w, v.query)
				n++
			}
			if (v.every !== undefined) {
				w.field(2)
				w.uint(v.every)
				n++
			}
			if (v.by !== undefined) {
				w.field(3)
				w.array(v.by.length)
				for (const item0 of v.by) {
					w.str(item0)
				}
				n++
			}
			if (v.stats !== undefined) {
				w.field(4)
				w.array(v.stats.length)
				for (const item0 of v.stats) {
					w.str(item0)
				}
				n++
			}
			if (v.distinct !== undefined) {
				w.field(5)
				w.array(v.distinct.length)
				for (const item0 of v.distinct) {
					w.str(item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $query: Read<typeof RecordsQuery.fields> | undefined
			let $every: number | undefined
			let $by: string[] | undefined
			let $stats: string[] | undefined
			let $distinct: string[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$query = RecordsQuery.read(r)
						break
					case 2:
						$every = r.uint()
						break
					case 3: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$by = items0
						break
					}
					case 4: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$stats = items0
						break
					}
					case 5: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$distinct = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { query: $query, every: $every, by: $by, stats: $stats, distinct: $distinct }
		},
	},
)

/**
 * A field's numbers in a row: a sum, a min and a max are JSON numbers, an
 * integer's exact.
 */
export const RecordsStats = message(
	'records.Stats',
	{
		samples: [1, uint],
		sum: [2, str],
		min: [3, str],
		max: [4, str],
		avg: [5, float],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.samples !== undefined) {
				w.field(1)
				w.uint(v.samples)
				n++
			}
			if (v.sum !== undefined) {
				w.field(2)
				w.str(v.sum)
				n++
			}
			if (v.min !== undefined) {
				w.field(3)
				w.str(v.min)
				n++
			}
			if (v.max !== undefined) {
				w.field(4)
				w.str(v.max)
				n++
			}
			if (v.avg !== undefined) {
				w.field(5)
				w.float(v.avg)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $samples: number | undefined
			let $sum: string | undefined
			let $min: string | undefined
			let $max: string | undefined
			let $avg: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$samples = r.uint()
						break
					case 2:
						$sum = r.str()
						break
					case 3:
						$min = r.str()
						break
					case 4:
						$max = r.str()
						break
					case 5:
						$avg = r.float()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { samples: $samples, sum: $sum, min: $min, max: $max, avg: $avg }
		},
	},
)

export const RecordsRow = message(
	'records.Row',
	{
		/** where the row's span begins, counted from 1970 */
		at: [1, int64],
		count: [2, uint],
		by: [3, names(str)],
		stats: [4, names(RecordsStats)],
		distinct: [5, names(uint)],
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
			if (v.count !== undefined) {
				w.field(2)
				w.uint(v.count)
				n++
			}
			if (v.by !== undefined) {
				w.field(3)
				writeNames(w, v.by, str.write)
				n++
			}
			if (v.stats !== undefined) {
				w.field(4)
				writeNames(w, v.stats, RecordsStats.write)
				n++
			}
			if (v.distinct !== undefined) {
				w.field(5)
				writeNames(w, v.distinct, uint.write)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $at: bigint | undefined
			let $count: number | undefined
			let $by: Record<string, string> | undefined
			let $stats: Record<string, Read<typeof RecordsStats.fields>> | undefined
			let $distinct: Record<string, number> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$at = r.int64()
						break
					case 2:
						$count = r.uint()
						break
					case 3:
						$by = readNames(r, str.read)
						break
					case 4:
						$stats = readNames(r, RecordsStats.read)
						break
					case 5:
						$distinct = readNames(r, uint.read)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { at: $at, count: $count, by: $by, stats: $stats, distinct: $distinct }
		},
	},
)

export const RecordsRows = message(
	'records.Rows',
	{
		rows: [1, list(RecordsRow)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.rows !== undefined) {
				w.field(1)
				w.array(v.rows.length)
				for (const item0 of v.rows) {
					RecordsRow.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $rows: Read<typeof RecordsRow.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof RecordsRow.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(RecordsRow.read(r))
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
			return { rows: $rows }
		},
	},
)

export const RecordsStream = message(
	'records.Stream',
	{
		name: [1, str],
		keep: [2, uint],
		/** its oldest event, or where its keep begins while it holds older */
		retainedFrom: [3, int64],
		newest: [4, int64],
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
			if (v.retainedFrom !== undefined) {
				w.field(3)
				w.int(v.retainedFrom)
				n++
			}
			if (v.newest !== undefined) {
				w.field(4)
				w.int(v.newest)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $keep: number | undefined
			let $retainedFrom: bigint | undefined
			let $newest: bigint | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$keep = r.uint()
						break
					case 3:
						$retainedFrom = r.int64()
						break
					case 4:
						$newest = r.int64()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, keep: $keep, retainedFrom: $retainedFrom, newest: $newest }
		},
	},
)

export const RecordsStreams = message(
	'records.Streams',
	{
		streams: [1, list(RecordsStream)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.streams !== undefined) {
				w.field(1)
				w.array(v.streams.length)
				for (const item0 of v.streams) {
					RecordsStream.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $streams: Read<typeof RecordsStream.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof RecordsStream.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(RecordsStream.read(r))
						}
						r.leave()
						$streams = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { streams: $streams }
		},
	},
)

/**
 * The keys the events a query finds hold, sorted.
 */
export const RecordsFieldNamesAsked = message(
	'records.FieldNamesAsked',
	{
		query: [1, RecordsQuery],
		prefix: [2, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.query !== undefined) {
				w.field(1)
				RecordsQuery.write(w, v.query)
				n++
			}
			if (v.prefix !== undefined) {
				w.field(2)
				w.str(v.prefix)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $query: Read<typeof RecordsQuery.fields> | undefined
			let $prefix: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$query = RecordsQuery.read(r)
						break
					case 2:
						$prefix = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { query: $query, prefix: $prefix }
		},
	},
)

export const RecordsNames = message(
	'records.Names',
	{
		names: [1, list(str)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.names !== undefined) {
				w.field(1)
				w.array(v.names.length)
				for (const item0 of v.names) {
					w.str(item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $names: string[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$names = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { names: $names }
		},
	},
)

/**
 * Every packed event once, a batch a call, from a cursor the program keeps.
 */
export const RecordsPollAsked = message(
	'records.PollAsked',
	{
		/** the oldest event kept without one */
		cursor: [1, str],
		/** 1000 unless given */
		limit: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.cursor !== undefined) {
				w.field(1)
				w.str(v.cursor)
				n++
			}
			if (v.limit !== undefined) {
				w.field(2)
				w.uint(v.limit)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $cursor: string | undefined
			let $limit: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$cursor = r.str()
						break
					case 2:
						$limit = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { cursor: $cursor, limit: $limit }
		},
	},
)

export const RecordsBatch = message(
	'records.Batch',
	{
		events: [1, list(RecordsEvent)],
		cursor: [2, str],
		/** the events keep removed before the cursor reached them */
		expired: [3, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.events !== undefined) {
				w.field(1)
				w.array(v.events.length)
				for (const item0 of v.events) {
					RecordsEvent.write(w, item0)
				}
				n++
			}
			if (v.cursor !== undefined) {
				w.field(2)
				w.str(v.cursor)
				n++
			}
			if (v.expired !== undefined) {
				w.field(3)
				w.uint(v.expired)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $events: Read<typeof RecordsEvent.fields>[] | undefined
			let $cursor: string | undefined
			let $expired: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof RecordsEvent.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(RecordsEvent.read(r))
						}
						r.leave()
						$events = items0
						break
					}
					case 2:
						$cursor = r.str()
						break
					case 3:
						$expired = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { events: $events, cursor: $cursor, expired: $expired }
		},
	},
)

/**
 * A tail's batch: the events appended since the last that its query finds,
 * and how many left the store's hold before the tail read them.
 */
export const RecordsTailBatch = message(
	'records.TailBatch',
	{
		events: [1, list(RecordsEvent)],
		missed: [2, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.events !== undefined) {
				w.field(1)
				w.array(v.events.length)
				for (const item0 of v.events) {
					RecordsEvent.write(w, item0)
				}
				n++
			}
			if (v.missed !== undefined) {
				w.field(2)
				w.uint(v.missed)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $events: Read<typeof RecordsEvent.fields>[] | undefined
			let $missed: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof RecordsEvent.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(RecordsEvent.read(r))
						}
						r.leave()
						$events = items0
						break
					}
					case 2:
						$missed = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { events: $events, missed: $missed }
		},
	},
)

/**
 * A piece of another program's output, cut anywhere.
 */
export const RecordsPiece = message(
	'records.Piece',
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
 * What a writer of lines dropped: lines past its buffer, longer than an
 * event holds, or refused by the file.
 */
export const RecordsDropped = message(
	'records.Dropped',
	{
		dropped: [1, uint],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.dropped !== undefined) {
				w.field(1)
				w.uint(v.dropped)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $dropped: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$dropped = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { dropped: $dropped }
		},
	},
)

/**
 * A head row or a segment that no longer reads, as damaged lists it and drop
 * takes it: a head row holds events not packed yet, a segment packed ones.
 */
export const RecordsDamage = message(
	'records.Damage',
	{
		/** head row or segment */
		part: [1, str],
		id: [2, uint],
		stream: [3, str],
		/** its events' oldest time */
		first: [4, int64],
		last: [5, int64],
		/** what broke */
		error: [6, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.part !== undefined) {
				w.field(1)
				w.str(v.part)
				n++
			}
			if (v.id !== undefined) {
				w.field(2)
				w.uint(v.id)
				n++
			}
			if (v.stream !== undefined) {
				w.field(3)
				w.str(v.stream)
				n++
			}
			if (v.first !== undefined) {
				w.field(4)
				w.int(v.first)
				n++
			}
			if (v.last !== undefined) {
				w.field(5)
				w.int(v.last)
				n++
			}
			if (v.error !== undefined) {
				w.field(6)
				w.str(v.error)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $part: string | undefined
			let $id: number | undefined
			let $stream: string | undefined
			let $first: bigint | undefined
			let $last: bigint | undefined
			let $error: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$part = r.str()
						break
					case 2:
						$id = r.uint()
						break
					case 3:
						$stream = r.str()
						break
					case 4:
						$first = r.int64()
						break
					case 5:
						$last = r.int64()
						break
					case 6:
						$error = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { part: $part, id: $id, stream: $stream, first: $first, last: $last, error: $error }
		},
	},
)

export const RecordsDamages = message(
	'records.Damages',
	{
		damages: [1, list(RecordsDamage)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.damages !== undefined) {
				w.field(1)
				w.array(v.damages.length)
				for (const item0 of v.damages) {
					RecordsDamage.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $damages: Read<typeof RecordsDamage.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof RecordsDamage.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(RecordsDamage.read(r))
						}
						r.leave()
						$damages = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { damages: $damages }
		},
	},
)

/**
 * A field a search reads, by its path into the document, and what a word
 * found there weighs.
 */
export const SearchField = message(
	'search.Field',
	{
		path: [1, str],
		/** above 0, at most 1000 */
		weight: [2, float],
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
			if (v.weight !== undefined) {
				w.field(2)
				w.float(v.weight)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $path: string | undefined
			let $weight: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$path = r.str()
						break
					case 2:
						$weight = r.float()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { path: $path, weight: $weight }
		},
	},
)

/**
 * A document set by its id, replacing it whole.
 */
export const SearchSet = message(
	'search.Set',
	{
		handle: [1, uint],
		/** 1 to 1024 bytes */
		id: [2, str],
		/** 1 MiB at most */
		value: [3, str],
		/** four bytes a dimension, little-endian floats */
		vector: [4, bin],
		/** the document has no vector, on an index with vectors */
		noVector: [5, bool],
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
			if (v.vector !== undefined) {
				w.field(4)
				w.bin(v.vector)
				n++
			}
			if (v.noVector !== undefined) {
				w.field(5)
				w.bool(v.noVector)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $id: string | undefined
			let $value: string | undefined
			let $vector: Uint8Array | undefined
			let $noVector: boolean | undefined
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
						$vector = r.bytes()
						break
					case 5:
						$noVector = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, id: $id, value: $value, vector: $vector, noVector: $noVector }
		},
	},
)

export const SearchDocument = message(
	'search.Document',
	{
		id: [1, str],
		value: [2, str],
		vector: [3, bin],
		noVector: [4, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.id !== undefined) {
				w.field(1)
				w.str(v.id)
				n++
			}
			if (v.value !== undefined) {
				w.field(2)
				w.str(v.value)
				n++
			}
			if (v.vector !== undefined) {
				w.field(3)
				w.bin(v.vector)
				n++
			}
			if (v.noVector !== undefined) {
				w.field(4)
				w.bool(v.noVector)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $id: string | undefined
			let $value: string | undefined
			let $vector: Uint8Array | undefined
			let $noVector: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$id = r.str()
						break
					case 2:
						$value = r.str()
						break
					case 3:
						$vector = r.bytes()
						break
					case 4:
						$noVector = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { id: $id, value: $value, vector: $vector, noVector: $noVector }
		},
	},
)

export const SearchKey = message(
	'search.Key',
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

export const SearchDeleted = message(
	'search.Deleted',
	{
		deleted: [1, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.deleted !== undefined) {
				w.field(1)
				w.bool(v.deleted)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $deleted: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$deleted = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { deleted: $deleted }
		},
	},
)

export const SearchGot = message(
	'search.Got',
	{
		found: [1, bool],
		value: [2, str],
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
				w.str(v.value)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $found: boolean | undefined
			let $value: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$found = r.bool()
						break
					case 2:
						$value = r.str()
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
 * A search: by `text`'s words, `vector`, or both, kept to what `where` passes.
 */
export const SearchQuery = message(
	'search.Query',
	{
		handle: [1, uint],
		text: [2, str],
		vector: [3, bin],
		/** this search's, in place of the index's */
		vectorWeight: [4, float],
		rerank: [5, bool],
		/** the conditions, as docs/wire.md's "search's where" writes them */
		where: [6, str],
		offset: [7, uint],
		/** 10 when absent; offset and limit together 1000 at most */
		limit: [8, uint],
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
			if (v.vector !== undefined) {
				w.field(3)
				w.bin(v.vector)
				n++
			}
			if (v.vectorWeight !== undefined) {
				w.field(4)
				w.float(v.vectorWeight)
				n++
			}
			if (v.rerank !== undefined) {
				w.field(5)
				w.bool(v.rerank)
				n++
			}
			if (v.where !== undefined) {
				w.field(6)
				w.str(v.where)
				n++
			}
			if (v.offset !== undefined) {
				w.field(7)
				w.uint(v.offset)
				n++
			}
			if (v.limit !== undefined) {
				w.field(8)
				w.uint(v.limit)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $text: string | undefined
			let $vector: Uint8Array | undefined
			let $vectorWeight: number | undefined
			let $rerank: boolean | undefined
			let $where: string | undefined
			let $offset: number | undefined
			let $limit: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$text = r.str()
						break
					case 3:
						$vector = r.bytes()
						break
					case 4:
						$vectorWeight = r.float()
						break
					case 5:
						$rerank = r.bool()
						break
					case 6:
						$where = r.str()
						break
					case 7:
						$offset = r.uint()
						break
					case 8:
						$limit = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				handle: $handle,
				text: $text,
				vector: $vector,
				vectorWeight: $vectorWeight,
				rerank: $rerank,
				where: $where,
				offset: $offset,
				limit: $limit,
			}
		},
	},
)

/**
 * A hit: a document and its value, or a table's row, a value a column of
 * the answer's columns, as sql.Rows holds one.
 */
export const SearchHit = message(
	'search.Hit',
	{
		id: [1, str],
		/** null for a table's row */
		value: [2, str],
		score: [3, float],
		row: [4, list(sqlValue)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.id !== undefined) {
				w.field(1)
				w.str(v.id)
				n++
			}
			if (v.value !== undefined) {
				w.field(2)
				w.str(v.value)
				n++
			}
			if (v.score !== undefined) {
				w.field(3)
				w.float(v.score)
				n++
			}
			if (v.row !== undefined) {
				w.field(4)
				w.array(v.row.length)
				for (const item0 of v.row) {
					sqlValue.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $id: string | undefined
			let $value: string | undefined
			let $score: number | undefined
			let $row: (SqlValue | boolean)[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$id = r.str()
						break
					case 2:
						$value = r.str()
						break
					case 3:
						$score = r.float()
						break
					case 4: {
						const items0: (SqlValue | boolean)[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(sqlValue.read(r))
						}
						r.leave()
						$row = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { id: $id, value: $value, score: $score, row: $row }
		},
	},
)

export const SearchFound = message(
	'search.Found',
	{
		hits: [1, list(SearchHit)],
		/** the text the search read in fact, when a word was read as another */
		corrected: [2, str],
		/** the columns of a table's index's rows */
		columns: [3, list(str)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.hits !== undefined) {
				w.field(1)
				w.array(v.hits.length)
				for (const item0 of v.hits) {
					SearchHit.write(w, item0)
				}
				n++
			}
			if (v.corrected !== undefined) {
				w.field(2)
				w.str(v.corrected)
				n++
			}
			if (v.columns !== undefined) {
				w.field(3)
				w.array(v.columns.length)
				for (const item0 of v.columns) {
					w.str(item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $hits: Read<typeof SearchHit.fields>[] | undefined
			let $corrected: string | undefined
			let $columns: string[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof SearchHit.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(SearchHit.read(r))
						}
						r.leave()
						$hits = items0
						break
					}
					case 2:
						$corrected = r.str()
						break
					case 3: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
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
			return { hits: $hits, corrected: $corrected, columns: $columns }
		},
	},
)

/**
 * Where an index stands: ready, rebuilding and how far, or failed and why.
 */
export const SearchStatus = message(
	'search.Status',
	{
		/** ready, rebuilding or failed */
		state: [1, str],
		/** the share a rebuild has read */
		done: [2, float],
		/** why a rebuild failed */
		error: [3, str],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.state !== undefined) {
				w.field(1)
				w.str(v.state)
				n++
			}
			if (v.done !== undefined) {
				w.field(2)
				w.float(v.done)
				n++
			}
			if (v.error !== undefined) {
				w.field(3)
				w.str(v.error)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $state: string | undefined
			let $done: number | undefined
			let $error: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$state = r.str()
						break
					case 2:
						$done = r.float()
						break
					case 3:
						$error = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { state: $state, done: $done, error: $error }
		},
	},
)

export const SearchName = message(
	'search.Name',
	{
		name: [1, str],
		/** a database's handle, for a table's index */
		database: [2, uint],
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
			if (v.database !== undefined) {
				w.field(2)
				w.uint(v.database)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $database: number | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$database = r.uint()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { name: $name, database: $database }
		},
	},
)

export const SearchDropped = message(
	'search.Dropped',
	{
		dropped: [1, bool],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.dropped !== undefined) {
				w.field(1)
				w.bool(v.dropped)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $dropped: boolean | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$dropped = r.bool()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { dropped: $dropped }
		},
	},
)

/**
 * An index as the store's search.db holds it.
 */
export const SearchIndex = message(
	'search.Index',
	{
		name: [1, str],
		documents: [2, uint],
		vectors: [3, uint],
		/** when a program last opened it */
		opened: [4, int],
		state: [5, str],
		done: [6, float],
		error: [7, str],
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
			if (v.documents !== undefined) {
				w.field(2)
				w.uint(v.documents)
				n++
			}
			if (v.vectors !== undefined) {
				w.field(3)
				w.uint(v.vectors)
				n++
			}
			if (v.opened !== undefined) {
				w.field(4)
				w.int(v.opened)
				n++
			}
			if (v.state !== undefined) {
				w.field(5)
				w.str(v.state)
				n++
			}
			if (v.done !== undefined) {
				w.field(6)
				w.float(v.done)
				n++
			}
			if (v.error !== undefined) {
				w.field(7)
				w.str(v.error)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $documents: number | undefined
			let $vectors: number | undefined
			let $opened: number | undefined
			let $state: string | undefined
			let $done: number | undefined
			let $error: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2:
						$documents = r.uint()
						break
					case 3:
						$vectors = r.uint()
						break
					case 4:
						$opened = r.int()
						break
					case 5:
						$state = r.str()
						break
					case 6:
						$done = r.float()
						break
					case 7:
						$error = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				name: $name,
				documents: $documents,
				vectors: $vectors,
				opened: $opened,
				state: $state,
				done: $done,
				error: $error,
			}
		},
	},
)

export const SearchListed = message(
	'search.Listed',
	{
		indexes: [1, list(SearchIndex)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.indexes !== undefined) {
				w.field(1)
				w.array(v.indexes.length)
				for (const item0 of v.indexes) {
					SearchIndex.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $indexes: Read<typeof SearchIndex.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1: {
						const items0: Read<typeof SearchIndex.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(SearchIndex.read(r))
						}
						r.leave()
						$indexes = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { indexes: $indexes }
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
 * A statement's text and the values of its ?s, in order. In a batch it may
 * be a call of jobs on a queue kept in the database's file instead, jobs.add
 * with its request in body and no text, which commits with the batch's rows.
 */
export const SqlText = message(
	'sql.Text',
	{
		text: [1, str],
		values: [2, list(sqlValue)],
		/** in a batch, jobs.add: the call in body */
		method: [3, uint],
		body: [4, bin],
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
			if (v.method !== undefined) {
				w.field(3)
				w.uint(v.method)
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
			let $text: string | undefined
			let $values: (SqlValue | boolean)[] | undefined
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
						$method = r.uint()
						break
					case 4:
						$body = r.bytes()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { text: $text, values: $values, method: $method, body: $body }
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
 * Writes known before they run, written as one in a shared commit: its
 * statements, and the adds of a queue kept in the database's file.
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

/**
 * What each write of a batch did, in their order: a job's add changed one
 * job, or none when its id was taken.
 */
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

/**
 * What a subscription receives at once: how many it missed since its last
 * batch, then messages in the order they were released.
 */
export const ChannelsBatch = message(
	'channels.Batch',
	{
		/** lost because it fell behind its branch's log */
		missed: [1, uint],
		messages: [2, list(ChannelsMessage)],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.missed !== undefined) {
				w.field(1)
				w.uint(v.missed)
				n++
			}
			if (v.messages !== undefined) {
				w.field(2)
				w.array(v.messages.length)
				for (const item0 of v.messages) {
					ChannelsMessage.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $missed: number | undefined
			let $messages: Read<typeof ChannelsMessage.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$missed = r.uint()
						break
					case 2: {
						const items0: Read<typeof ChannelsMessage.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(ChannelsMessage.read(r))
						}
						r.leave()
						$messages = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { missed: $missed, messages: $messages }
		},
	},
)

/**
 * A config's value at a version of its changes, and where each setting's
 * value came from.
 */
export const ConfigValue = message(
	'config.Value',
	{
		handle: [1, uint],
		version: [2, int],
		/** the settings, each span its milliseconds, each secret its text */
		value: [3, str],
		lines: [4, list(ConfigLine)],
		/** what was read and not used, each with why */
		notes: [5, list(str)],
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
			if (v.version !== undefined) {
				w.field(2)
				w.int(v.version)
				n++
			}
			if (v.value !== undefined) {
				w.field(3)
				w.str(v.value)
				n++
			}
			if (v.lines !== undefined) {
				w.field(4)
				w.array(v.lines.length)
				for (const item0 of v.lines) {
					ConfigLine.write(w, item0)
				}
				n++
			}
			if (v.notes !== undefined) {
				w.field(5)
				w.array(v.notes.length)
				for (const item0 of v.notes) {
					w.str(item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $version: number | undefined
			let $value: string | undefined
			let $lines: Read<typeof ConfigLine.fields>[] | undefined
			let $notes: string[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2:
						$version = r.int()
						break
					case 3:
						$value = r.str()
						break
					case 4: {
						const items0: Read<typeof ConfigLine.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(ConfigLine.read(r))
						}
						r.leave()
						$lines = items0
						break
					}
					case 5: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$notes = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, version: $version, value: $value, lines: $lines, notes: $notes }
		},
	},
)

/**
 * What a call would spend of each bound, and the bound it would stop at.
 */
export const MetricsPlan = message(
	'metrics.Plan',
	{
		series: [1, uint],
		bytes: [2, uint],
		decoded: [3, uint],
		exceeds: [4, MetricsExceeds],
	},
	{
		write(w, v) {
			const head = w.openMap()
			let n = 0
			if (v.series !== undefined) {
				w.field(1)
				w.uint(v.series)
				n++
			}
			if (v.bytes !== undefined) {
				w.field(2)
				w.uint(v.bytes)
				n++
			}
			if (v.decoded !== undefined) {
				w.field(3)
				w.uint(v.decoded)
				n++
			}
			if (v.exceeds !== undefined) {
				w.field(4)
				MetricsExceeds.write(w, v.exceeds)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $series: number | undefined
			let $bytes: number | undefined
			let $decoded: number | undefined
			let $exceeds: Read<typeof MetricsExceeds.fields> | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$series = r.uint()
						break
					case 2:
						$bytes = r.uint()
						break
					case 3:
						$decoded = r.uint()
						break
					case 4:
						$exceeds = MetricsExceeds.read(r)
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return { series: $series, bytes: $bytes, decoded: $decoded, exceeds: $exceeds }
		},
	},
)

/**
 * Opens an index, made the first time; one that holds other settings is
 * changed to these, as docs/guides/search/rebuilds.md has it. A read
 * connection opens an index only as it is. With a database, the index
 * follows a table of it: its fields and filters are columns.
 */
export const SearchOpen = message(
	'search.Open',
	{
		/** [a-z0-9][a-z0-9_-]{0,63} */
		name: [1, str],
		/** in order; none for a document that is its own text */
		fields: [2, list(SearchField)],
		/** the fields a search may filter by */
		filters: [3, list(str)],
		/** on when absent */
		typoTolerance: [4, bool],
		/** the vectors' dimensions; none for an index without vectors */
		dimensions: [5, uint],
		/** above 0 and below 1; exact when absent */
		recall: [6, float],
		/** 0.5 when absent */
		vectorWeight: [7, float],
		rerank: [8, bool],
		/** a change that needs reading again waits for reindex */
		manual: [9, bool],
		/** reads the index again beside itself */
		reindex: [10, bool],
		/** a database's handle: the index follows a table in its file */
		database: [11, uint],
		/** the table a database's index follows */
		table: [12, str],
		/** the table's column of unique values; its primary key when absent */
		key: [13, str],
		/** the column of each row's vector, a blob of little-endian floats */
		vectorColumn: [14, str],
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
			if (v.fields !== undefined) {
				w.field(2)
				w.array(v.fields.length)
				for (const item0 of v.fields) {
					SearchField.write(w, item0)
				}
				n++
			}
			if (v.filters !== undefined) {
				w.field(3)
				w.array(v.filters.length)
				for (const item0 of v.filters) {
					w.str(item0)
				}
				n++
			}
			if (v.typoTolerance !== undefined) {
				w.field(4)
				w.bool(v.typoTolerance)
				n++
			}
			if (v.dimensions !== undefined) {
				w.field(5)
				w.uint(v.dimensions)
				n++
			}
			if (v.recall !== undefined) {
				w.field(6)
				w.float(v.recall)
				n++
			}
			if (v.vectorWeight !== undefined) {
				w.field(7)
				w.float(v.vectorWeight)
				n++
			}
			if (v.rerank !== undefined) {
				w.field(8)
				w.bool(v.rerank)
				n++
			}
			if (v.manual !== undefined) {
				w.field(9)
				w.bool(v.manual)
				n++
			}
			if (v.reindex !== undefined) {
				w.field(10)
				w.bool(v.reindex)
				n++
			}
			if (v.database !== undefined) {
				w.field(11)
				w.uint(v.database)
				n++
			}
			if (v.table !== undefined) {
				w.field(12)
				w.str(v.table)
				n++
			}
			if (v.key !== undefined) {
				w.field(13)
				w.str(v.key)
				n++
			}
			if (v.vectorColumn !== undefined) {
				w.field(14)
				w.str(v.vectorColumn)
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $name: string | undefined
			let $fields: Read<typeof SearchField.fields>[] | undefined
			let $filters: string[] | undefined
			let $typoTolerance: boolean | undefined
			let $dimensions: number | undefined
			let $recall: number | undefined
			let $vectorWeight: number | undefined
			let $rerank: boolean | undefined
			let $manual: boolean | undefined
			let $reindex: boolean | undefined
			let $database: number | undefined
			let $table: string | undefined
			let $key: string | undefined
			let $vectorColumn: string | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$name = r.str()
						break
					case 2: {
						const items0: Read<typeof SearchField.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(SearchField.read(r))
						}
						r.leave()
						$fields = items0
						break
					}
					case 3: {
						const items0: string[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(r.str())
						}
						r.leave()
						$filters = items0
						break
					}
					case 4:
						$typoTolerance = r.bool()
						break
					case 5:
						$dimensions = r.uint()
						break
					case 6:
						$recall = r.float()
						break
					case 7:
						$vectorWeight = r.float()
						break
					case 8:
						$rerank = r.bool()
						break
					case 9:
						$manual = r.bool()
						break
					case 10:
						$reindex = r.bool()
						break
					case 11:
						$database = r.uint()
						break
					case 12:
						$table = r.str()
						break
					case 13:
						$key = r.str()
						break
					case 14:
						$vectorColumn = r.str()
						break
					default:
						r.skip()
				}
			}
			r.leave()
			return {
				name: $name,
				fields: $fields,
				filters: $filters,
				typoTolerance: $typoTolerance,
				dimensions: $dimensions,
				recall: $recall,
				vectorWeight: $vectorWeight,
				rerank: $rerank,
				manual: $manual,
				reindex: $reindex,
				database: $database,
				table: $table,
				key: $key,
				vectorColumn: $vectorColumn,
			}
		},
	},
)

/**
 * Documents set in one commit, all of them or none: 1,000 and 16 MiB at most.
 */
export const SearchSetMany = message(
	'search.SetMany',
	{
		handle: [1, uint],
		documents: [2, list(SearchDocument)],
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
			if (v.documents !== undefined) {
				w.field(2)
				w.array(v.documents.length)
				for (const item0 of v.documents) {
					SearchDocument.write(w, item0)
				}
				n++
			}
			w.closeMap(head, n)
		},
		read(r) {
			let $handle: number | undefined
			let $documents: Read<typeof SearchDocument.fields>[] | undefined
			for (let n = r.message(); n > 0; n--) {
				switch (r.field()) {
					case 1:
						$handle = r.uint()
						break
					case 2: {
						const items0: Read<typeof SearchDocument.fields>[] = []
						for (let i0 = r.array(); i0 > 0; i0--) {
							items0.push(SearchDocument.read(r))
						}
						r.leave()
						$documents = items0
						break
					}
					default:
						r.skip()
				}
			}
			r.leave()
			return { handle: $handle, documents: $documents }
		},
	},
)
