// What open says when it cannot reach a store, through the core in this
// process, a private child and a sidecar: a directory that is a file, a
// binary that is not there, and a sidecar that fails before it serves; and
// an open, a connect or a call its caller gives up on.

import { expect, test } from 'bun:test'
import { spawnSync } from 'node:child_process'
import { randomBytes } from 'node:crypto'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'

import { startFailure } from '../src/connection.ts'
import {
	CancelledError,
	ClosedError,
	connect,
	InvalidError,
	open,
	type Store,
	withSignal,
} from '../src/index.ts'
import { protocol } from '../src/session.ts'
import { caught, removed, until, ways } from './ways.ts'

const binary = process.env.TINYSTORE_BIN
const served = binary !== undefined && binary !== 'unused'

/** The ways a store opens from its directory: every one but a remote server's. */
const local = ways.filter(way => way.name !== 'a remote server over TCP' && way.ready)

function scratch(name: string): string {
	return mkdtempSync(join(tmpdir(), `tinystore-${name}-`))
}

// contract: ERR-03, WIRE-01
test('a directory that is a file, or sits under one, is InvalidError naming the file, in every way', async () => {
	const base = scratch('file')
	const file = join(base, 'data')
	writeFileSync(file, 'not a directory')
	const under = join(file, 'store')
	for (const way of local) {
		const itself = await caught(way.open(file))
		expect(itself, way.name).toBeInstanceOf(InvalidError)
		expect((itself as Error).message, way.name).toBe(`store ${file} is a file, not a directory`)
		const below = await caught(way.open(under))
		expect(below, way.name).toBeInstanceOf(InvalidError)
		expect((below as Error).message, way.name).toBe(
			`store ${under}: ${file} is a file, not a directory`,
		)
	}
	await removed(base)
})

// contract: ERR-02
test.skipIf(!served)(
	'a binary that is not there is named, for a sidecar and a private child',
	async () => {
		const base = scratch('binary')
		const missing = join(base, process.platform === 'win32' ? 'nope.exe' : 'nope')
		const dir = join(base, 'store')
		const sidecar = await caught(open(dir, { binary: missing }))
		expect(sidecar).toBeInstanceOf(ClosedError)
		expect((sidecar as Error).message).toBe(`cannot start the sidecar ${missing}: not found`)
		const child = await caught(open(dir, { private: true, binary: missing }))
		expect(child).toBeInstanceOf(ClosedError)
		expect((child as Error).message).toBe(`cannot start the private server ${missing}: not found`)
		await removed(base)
	},
)

// contract: ERR-02
test('a sidecar that fails before it serves says what it wrote to stderr, not what an older run logged', async () => {
	const dir = scratch('stderr')
	mkdirSync(join(dir, 'server'))
	writeFileSync(
		join(dir, 'server', 'serve.log'),
		'{"level":"INFO","msg":"stopped","took":"0.1s"}\n',
	)
	// this runtime's executable stands in for tinystore: given serve --dir …,
	// it fails, says why on stderr and writes no log
	const failed = await caught(open(dir, { binary: process.execPath }))
	expect(failed).toBeInstanceOf(ClosedError)
	const message = (failed as Error).message
	expect(message).toMatch(/^the sidecar exited with [1-9]\d*: /)
	expect(message).not.toContain('stopped')
	expect(message).not.toContain('it wrote no log')
	await removed(dir)
})

// contract: ERR-02
test('a sidecar that wrote nothing to stderr is told by the lines its log gained since it started, or by none', () => {
	const older = '{"level":"INFO","msg":"stopped"}\n'
	const bytes = (text: string) => new TextEncoder().encode(text)
	const failing = '{"level":"ERROR","msg":"no space left"}'
	expect(startFailure('', bytes(`${older}${failing}\n`), older.length)).toBe(failing)
	expect(startFailure('', bytes(older), older.length)).toBe('it wrote no log')
	expect(startFailure('', undefined, 0)).toBe('it wrote no log')
	// a log shorter than it was is another file, every line of it new
	expect(startFailure('', bytes(`${failing}\n`), older.length * 2)).toBe(failing)
	expect(startFailure('it fails\n', bytes(`${older}${failing}\n`), 0)).toBe('it fails')
})

// contract: ERR-02
test("a node's config that cannot be written is named, before any sidecar starts", async () => {
	const dir = scratch('node-config')
	// a file where the server's folder goes
	writeFileSync(join(dir, 'server'), 'not a folder')
	const config = join(dir, 'server', 'node.toml')
	const failed = await caught(open(dir, { node: { files: ['kv.db'] } }))
	expect(failed).toBeInstanceOf(ClosedError)
	expect((failed as Error).message).toStartWith(`cannot write the node's config ${config}: `)
	await removed(dir)
})

// contract: ERR-02
test('a private child that exits before it answers says what it wrote to stderr, once', async () => {
	const dir = scratch('child')
	const failed = await caught(open(dir, { private: true, binary: process.execPath }))
	expect(failed).toBeInstanceOf(ClosedError)
	const message = (failed as Error).message
	const exited = 'the private server did not start: the private server exited with '
	expect(message).toStartWith(exited)
	const said = message.slice(exited.length).split(': ').slice(1).join(': ')
	expect(said).not.toBe('')
	expect(message.split(said).length).toBe(2)
	await removed(dir)
})

/** A store whose sidecar was stopped and whose directory another process now holds: its next call dials, and waits. */
async function waitingToRedial(
	name: string,
): Promise<{ dir: string; store: Store; holder: Store }> {
	const dir = scratch(name)
	const store = await open(dir, { idle: '1s' })
	await store.bucket<string>('notes').set('a', 'kept')
	const stopped = spawnSync(binary!, ['stop', dir], { encoding: 'utf8' })
	expect(stopped.status, stopped.stderr).toBe(0)
	// the stopped sidecar lets go of the directory a moment after its SERVE goes
	let holder: Store | undefined
	await until(async () => {
		holder = await open(dir, { private: true }).catch(() => undefined)
		return holder !== undefined
	})
	expect(holder).toBeDefined()
	return { dir, store, holder: holder! }
}

// contract: WIRE-11
test.skipIf(!served)(
	'an open cancelled while another process holds the directory stops at once, and starts nothing after',
	async () => {
		const dir = scratch('held')
		const holder = await open(dir, { private: true })
		const started = Date.now()
		const failed = await caught(
			withSignal(AbortSignal.timeout(1000), () => open(dir, { idle: '1s' })),
		)
		const took = Date.now() - started
		expect(failed).toBeInstanceOf(CancelledError)
		expect((failed as Error).message).toBe(
			`open ${dir}: cancelled while another process holds the directory and no sidecar answers`,
		)
		expect(took).toBeLessThan(1500)
		// every sidecar the open started found the directory held and left; none starts once it is free
		await sleep(1000)
		await holder.close()
		await sleep(1500)
		expect(existsSync(join(dir, 'server', 'SERVE'))).toBe(false)
		await removed(dir)
	},
	30_000,
)

// contract: WIRE-11
test.skipIf(!served)(
	'an open cancelled while the server SERVE names says nothing starts no sidecar after',
	async () => {
		const silent = createServer(socket => {
			socket.on('data', () => {})
			socket.on('error', () => {})
		})
		await new Promise<void>(listening => silent.listen(0, '127.0.0.1', listening))
		const { port } = silent.address() as { port: number }
		const dir = scratch('probe')
		mkdirSync(join(dir, 'server'))
		const serve = join(dir, 'server', 'SERVE')
		const published = JSON.stringify({
			protocol,
			endpoints: [`tcp://127.0.0.1:${port}`],
			secret: randomBytes(32).toString('base64url'),
			sidecar: true,
			instance: 'silent',
		})
		writeFileSync(serve, published)
		const failed = await caught(
			withSignal(AbortSignal.timeout(500), () => open(dir, { idle: '1s' })),
		)
		expect(failed).toBeInstanceOf(CancelledError)
		expect((failed as Error).message).toBe(`open ${dir}: cancelled while the sidecar starts`)
		// a sidecar started after the cancel opens its log and publishes itself well within this
		await sleep(1500)
		const started = existsSync(join(dir, 'server', 'serve.log'))
		if (started) {
			spawnSync(binary!, ['stop', dir])
		}
		expect(started).toBe(false)
		expect(readFileSync(serve, 'utf8')).toBe(published)
		silent.close()
		await removed(dir)
	},
	30_000,
)

// contract: WIRE-11
test('a connect cancelled while its server says nothing stops at once, and lets the socket go', async () => {
	let ended = false
	const silent = createServer(socket => {
		socket.on('data', () => {})
		socket.on('close', () => {
			ended = true
		})
	})
	await new Promise<void>(listening => silent.listen(0, '127.0.0.1', listening))
	const { port } = silent.address() as { port: number }
	const started = Date.now()
	const failed = await caught(
		withSignal(AbortSignal.timeout(500), () =>
			connect(`tcp://127.0.0.1:${port}`, { token: 'unused' }),
		),
	)
	expect(failed).toBeInstanceOf(CancelledError)
	expect((failed as Error).message).toBe(
		`connect tcp://127.0.0.1:${port}: cancelled while the server has not answered`,
	)
	expect(Date.now() - started).toBeLessThan(1000)
	await until(() => ended)
	expect(ended).toBe(true)
	expect(Date.now() - started).toBeLessThan(1500)
	silent.close()
}, 30_000)

// contract: WIRE-11
test.skipIf(!served)(
	'a call stops waiting for its store to dial again when its signal aborts, and an aborted signal is CancelledError',
	async () => {
		const { dir, store, holder } = await waitingToRedial('redial')
		const notes = store.bucket<string>('notes')
		const aborted = await caught(withSignal(AbortSignal.abort(), () => notes.get('a')))
		expect(aborted).toBeInstanceOf(CancelledError)
		const started = Date.now()
		const failed = await caught(withSignal(AbortSignal.timeout(500), () => notes.get('a')))
		expect(failed).toBeInstanceOf(CancelledError)
		expect(Date.now() - started).toBeLessThan(1500)
		await store.close()
		await holder.close()
		await removed(dir)
	},
	30_000,
)

// contract: WIRE-11
test.skipIf(!served)(
	'a store closed while it dials returns at once, and a call that waited fails ClosedError',
	async () => {
		const { dir, store, holder } = await waitingToRedial('closed')
		const waiting = caught(store.bucket<string>('notes').get('a'))
		await sleep(300)
		const started = Date.now()
		await store.close()
		expect(Date.now() - started).toBeLessThan(1000)
		const failed = await waiting
		expect(failed).toBeInstanceOf(ClosedError)
		expect((failed as Error).message).toBe('the store is closed')
		await holder.close()
		await removed(dir)
	},
	30_000,
)
