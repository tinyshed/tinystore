// What open says when it cannot reach a store, through the core in this
// process, a private child and a sidecar: a directory that is a file, a
// binary that is not there, and a sidecar that fails before it serves.

import { expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { startFailure } from '../src/connection.ts'
import { ClosedError, InvalidError, open } from '../src/index.ts'
import { caught, removed, ways } from './ways.ts'

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
