// A backup of the whole store, as docs/guides/backup/README.md has it: one zip of every
// engine that the server writes while the store keeps working and sends in
// pieces, written beside the name the program gives and renamed once whole,
// so that a backup that fails leaves no zip.

import { open, rename, rm } from 'node:fs/promises'

import type { Link } from './connection.ts'
import { BackupPiece, BackupWrite, BackupWritten, methods } from './wire/protocol.ts'

/** What a backup takes beside every engine's files. */
export interface BackupOptions {
	/**
	 * files of the program's it includes, by their paths inside the store's
	 * directory, such as 'secret.key', which a restore puts back where they
	 * were: a backup holds none unless named
	 */
	include?: readonly string[]
	/**
	 * includes the store's encryption key, as it is, which a backup leaves
	 * out otherwise: the zip is not encrypted, so a backup with the key opens
	 * every value it holds
	 */
	includeEncryptionKey?: boolean
}

/** What a backup holds. */
export interface Backup {
	/** the files, the engines' and those it was told to take */
	files: number
	/** their bytes, the zip's own fields aside */
	bytes: number
	/**
	 * the store seals values with an encryption key the backup does not
	 * hold: they open with that key alone, kept apart
	 */
	keyLeftOut: boolean
}

const nothing = new Uint8Array()

/**
 * Writes a backup of the store `link` reaches to `path`, beside it first:
 * a connection lost on the way makes it again from its start.
 */
export async function backup(link: Link, path: string, options: BackupOptions): Promise<Backup> {
	const partial = `${path}.${process.pid}.part`
	const asked = BackupWrite.encode({
		include: [...(options.include ?? [])],
		includeEncryptionKey: options.includeEncryptionKey === true,
	})
	try {
		const backed = await link.run('read', async connection => {
			const file = await open(partial, 'w')
			try {
				const stream = await connection.session.open(methods['backup.write'], asked, true)
				await stream.next()
				for (;;) {
					const event = await stream.next()
					if (event.end) {
						await file.sync()
						return BackupWritten.decode(event.body)
					}
					stream.consumed(event.body.length)
					await file.write(BackupPiece.decode(event.body).bytes ?? nothing)
				}
			} finally {
				await file.close()
			}
		})
		await rename(partial, path)
		return {
			files: Number(backed.files ?? 0),
			bytes: Number(backed.bytes ?? 0),
			keyLeftOut: backed.keyLeftOut === true,
		}
	} catch (err) {
		await rm(partial, { force: true })
		throw err
	}
}
