// A directory, as docs/guides/blobs/directories.md has it: a folder of the
// store's machine under a name, which the store lists and reads and never
// writes. A scan writes down what appeared, changed or went; changes keep a
// copy in step from a cursor; a read comes from the disk, held to the version
// its get opened, or in a growing directory to none.

import {
	type FileInfo,
	type FileListOptions,
	type FilePage,
	infoOf,
	readStream,
	StoredFile,
	segmentOf,
} from './blobs.ts'
import type { Connection, Link } from './connection.ts'
import { checkName, handleOn, type Via, viaLink } from './handles.ts'
import { type Duration, ms } from './time.ts'
import {
	BlobsAt,
	BlobsChanges,
	BlobsDirectory,
	BlobsFolder,
	BlobsGot,
	BlobsHead,
	BlobsList,
	BlobsPage,
	BlobsScanned,
	BlobsSince,
	BlobsUsage,
	methods,
} from './wire/protocol.ts'

export interface DirectoryOptions {
	/** The folder on the store's machine; a server's own, by the name, when left out. */
	path?: string | undefined
	/** A scan on the store's background work, a period apart: minutes, not seconds. */
	every?: Duration | undefined
	/** Names a scan passes over, a file's or a folder's, `*` for any run of a name's characters. */
	ignore?: string[] | undefined
	/** For files written while they are read: a read holds to no version and goes up to the size now. */
	growing?: boolean | undefined
}

/** What a scan found, and what the list holds once it ended. */
export interface Scan {
	added: number
	changed: number
	removed: number
	/** what it met and could not list: a link, a folder it cannot read, another disk */
	skipped: number
	count: number
	size: number
}

export type Change = 'added' | 'changed' | 'removed'

/** A file's change since a cursor: its path in the directory, and what it carries now, none once removed. */
export interface FileChange {
	path: string
	change: Change
	info: FileInfo | undefined
}

/** A page of changes in the order the scans wrote them; `next` is always there. */
export interface DirectoryChanges {
	items: FileChange[]
	next: string
}

export function openDirectory(link: Link, name: string, options?: DirectoryOptions): Directory {
	checkName(name, 'directory')
	const open = BlobsDirectory.encode({
		name,
		path: options?.path,
		every: options?.every === undefined ? undefined : ms(options.every),
		ignore: options?.ignore ?? [],
		growing: options?.growing === true,
	})
	return new Directory(link, name, open, [], options?.growing === true)
}

/**
 * A directory, or a folder of it: `get`, `head`, `list` and `usage` are a set
 * of files' calls with a set of files' answers, answered from the list the
 * last scan wrote but a get's bytes, which come from the disk.
 */
export class Directory {
	readonly name: string
	readonly #link: Link
	readonly #via: Via
	readonly #open: Uint8Array
	readonly #folder: string[]
	readonly #growing: boolean

	/** Directories come from `store.directory`. */
	constructor(link: Link, name: string, open: Uint8Array, folder: string[], growing: boolean) {
		this.name = name
		this.#link = link
		this.#via = viaLink(link)
		this.#open = open
		this.#folder = folder
		this.#growing = growing
	}

	/** The folder of these segments, one each, whose paths are relative to it. */
	folder(...segments: (string | number | bigint)[]): Directory {
		const folder = [...this.#folder, ...segments.map(segmentOf)]
		return new Directory(this.#link, this.name, this.#open, folder, this.#growing)
	}

	/** Walks the folder and writes down what appeared, changed or went since the last scan. */
	async scan(): Promise<Scan> {
		const answer = await this.#call(
			'blobs.scan',
			handle => BlobsFolder.encode({ handle, folder: this.#folder }),
			'write',
		)
		const scan = BlobsScanned.decode(answer)
		return {
			added: scan.added ?? 0,
			changed: scan.changed ?? 0,
			removed: scan.removed ?? 0,
			skipped: scan.skipped ?? 0,
			count: scan.count ?? 0,
			size: scan.size ?? 0,
		}
	}

	/** The changes past a cursor, a page's `next`, or from the first file the directory listed. */
	async changes(options?: { after?: string | undefined }): Promise<DirectoryChanges> {
		const answer = await this.#call(
			'blobs.changes',
			handle => BlobsSince.encode({ handle, after: options?.after }),
			'read',
		)
		const page = BlobsChanges.decode(answer)
		const items = (page.items ?? []).map(item => ({
			path: item.path ?? '',
			change: (item.change ?? 'added') as Change,
			info: item.info === undefined ? undefined : infoOf(item.info),
		}))
		return { items, next: page.next ?? '0' }
	}

	/**
	 * The file as a scan listed it and as the disk holds it now; undefined when
	 * no scan listed it, or it is gone since. A read of a file that moved since
	 * fails ConflictError, but in a growing directory, which reads up to the size now.
	 */
	async get(path: string): Promise<StoredFile | undefined> {
		const answer = await this.#call(
			'blobs.get',
			handle => BlobsAt.encode({ handle, folder: this.#folder, path }),
			'read',
		)
		const got = BlobsGot.view(answer)
		if (got.info === undefined) {
			return undefined
		}
		const info = infoOf(got.info)
		const bytes = got.bytes ?? new Uint8Array()
		// a growing file's bytes come from the disk as it is when they are read
		const whole = !this.#growing && bytes.length === info.size ? bytes : undefined
		const ifMatch = this.#growing ? undefined : info.etag
		const handle = (connection: Connection) =>
			handleOn(connection, methods['blobs.directory'], this.#open)
		return new StoredFile(info, whole, (offset, length) =>
			readStream(this.#link, handle, { folder: this.#folder, path, ifMatch, offset, length }),
		)
	}

	/** What the last scan wrote down of the file; it reads no disk. */
	async head(path: string): Promise<FileInfo | undefined> {
		const answer = await this.#call(
			'blobs.head',
			handle => BlobsAt.encode({ handle, folder: this.#folder, path }),
			'read',
		)
		const head = BlobsHead.decode(answer)
		return head.info === undefined ? undefined : infoOf(head.info)
	}

	/** A page of the folder's files, in the byte order of their paths, as the last scan wrote them. */
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

	/** The files the last scan listed in the folder, and their bytes. */
	async usage(): Promise<{ count: number; size: number }> {
		const answer = await this.#call(
			'blobs.usage',
			handle => BlobsFolder.encode({ handle, folder: this.#folder }),
			'read',
		)
		const usage = BlobsUsage.decode(answer)
		return { count: usage.count ?? 0, size: usage.size ?? 0 }
	}

	/** Removes every file of the list, each told removed, and nothing on the disk. */
	async forget(): Promise<void> {
		await this.#call(
			'blobs.forget',
			handle => BlobsFolder.encode({ handle, folder: this.#folder }),
			'write',
		)
	}

	#call(
		method: keyof typeof methods,
		body: (handle: number) => Uint8Array,
		idempotence: 'read' | 'write',
	): Promise<Uint8Array> {
		return this.#via.call(
			methods['blobs.directory'],
			this.#open,
			methods[method],
			body,
			idempotence,
		)
	}
}
