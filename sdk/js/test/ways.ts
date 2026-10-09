// Each way a store is reached, for the suites that run through all of them:
// the core in this process, when TINYSTORE_LIBRARY names the library `cargo
// build -p tinystore-ffi` made, and a private child, a sidecar and a remote
// server, when TINYSTORE_BIN names the tinystore that test/binary.ts or `just
// sdk` built.

import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

import { connect, open, type Store } from '../src/index.ts'

const library = process.env.TINYSTORE_LIBRARY
const binary = process.env.TINYSTORE_BIN
const served = binary !== undefined && binary !== 'unused'

/** A remote server's admin token, in a tokens file of the test's own. */
const token = '4dGQ6tR2kq0SxVZpLWn8E1yHf7cJmB3aUoN9Tz5Ki0E'

/** Each way a store is reached: how a test opens one in a directory, and lets go of it after. */
export interface Way {
	name: string
	ready: boolean
	open(dir: string): Promise<Store>
	/** Waits until whatever served the directory has let go of it. */
	leave(dir: string): Promise<void>
}

const remotes = new Map<string, ReturnType<typeof Bun.spawn>>()

export const ways: Way[] = [
	{
		name: 'the core in this process',
		ready: library !== undefined,
		open: dir => open(dir, { embedded: true, library }),
		leave: async () => {},
	},
	{
		name: 'a private child',
		ready: served,
		open: dir => open(dir, { private: true }),
		leave: async () => {},
	},
	{
		name: 'a sidecar',
		ready: served,
		// gone at once once the store closes, so that its directory can go
		open: dir => open(dir, { idle: 1 }),
		leave: dir => until(() => !existsSync(join(dir, 'server', 'SERVE'))),
	},
	{
		name: 'a remote server over TCP',
		ready: served,
		open: async dir => {
			writeFileSync(join(dir, 'tokens'), `admin ${token}\n`)
			const args = [
				'serve',
				join(dir, 'store'),
				'--listen',
				'tcp://127.0.0.1:0',
				'--tokens',
				join(dir, 'tokens'),
			]
			remotes.set(dir, Bun.spawn([binary!, ...args], { stdout: 'ignore', stderr: 'ignore' }))
			const serve = join(dir, 'store', 'server', 'SERVE')
			await until(() => existsSync(serve))
			const endpoints: string[] = JSON.parse(readFileSync(serve, 'utf8')).endpoints
			return connect(endpoints[1]!, { token })
		},
		leave: async dir => {
			const server = remotes.get(dir)
			server?.kill()
			await server?.exited
		},
	},
]

export async function until(done: () => boolean | Promise<boolean>): Promise<void> {
	const deadline = Date.now() + 10_000
	while (!(await done()) && Date.now() < deadline) {
		await Bun.sleep(20)
	}
}

/**
 * What a promise rejected with. Bun's expect(...).rejects waits for a
 * promise without running the event loop's I/O, so a call answered by the
 * core never settles inside it.
 */
export async function caught(promise: Promise<unknown>): Promise<unknown> {
	return promise.then(
		() => undefined,
		(err: unknown) => err,
	)
}

/** Removes a directory, again while Windows still holds a file in it a moment after its server. */
export async function removed(dir: string): Promise<void> {
	const deadline = Date.now() + 10_000
	for (;;) {
		try {
			rmSync(dir, { recursive: true, force: true })
			return
		} catch (err) {
			if (Date.now() > deadline) {
				throw err
			}
			await Bun.sleep(20)
		}
	}
}
