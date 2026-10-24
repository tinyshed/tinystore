// The SDK under Node, through Node's runtime: the core in this process
// through its addon, when TINYSTORE_ADDON names the tinystore.node that just
// sdk-node made, and a real tinystore as the sidecar, a private child and a
// remote server over TCP and TLS. Node runs the TypeScript as it is: node
// --test test/under-node.ts.

import assert from 'node:assert/strict'
import { type ChildProcess, spawn, spawnSync } from 'node:child_process'
import { randomBytes } from 'node:crypto'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { after, before, test } from 'node:test'
import { setTimeout as sleep } from 'node:timers/promises'

import { connect, open } from '../src/index.ts'
import { nodeRuntime } from '../src/runtime/node.ts'

const scratch = mkdtempSync(join(tmpdir(), 'tinystore-node-'))
const servers: ChildProcess[] = []

const addon = process.env.TINYSTORE_ADDON

before(() => {
	assert.ok(
		process.env.TINYSTORE_BIN !== undefined,
		'TINYSTORE_BIN names the tinystore that just build made',
	)
})

// on Windows a server still exiting holds its files, and so does the sidecar
// until its idle time has passed: the removal is tried again meanwhile, since
// rmSync's own retries pass over EPERM
after(async () => {
	await Promise.all(
		servers.map(server => {
			const exited = new Promise(done => server.once('exit', done))
			return server.exitCode === null && server.kill() ? exited : undefined
		}),
	)
	for (let tried = 1; ; tried++) {
		try {
			rmSync(scratch, { recursive: true, force: true })
			return
		} catch (err) {
			if (tried === 100) {
				throw err
			}
			await sleep(100)
		}
	}
})

test('the runtime is Node’s, and finds what is on PATH', () => {
	assert.match(nodeRuntime.client, /^tinystore-js\/\S+ node\/\d+/)
	assert.ok(nodeRuntime.which('node') !== undefined)
	assert.equal(nodeRuntime.which('no-such-program-of-tinystore'), undefined)
})

test('the core in this process answers under Node: values, many writes, a file, a message, a job', {
	skip: addon === undefined && 'TINYSTORE_ADDON names no tinystore.node',
}, async () => {
	const dir = join(scratch, 'embedded')
	const store = await open(dir, { embedded: true, library: addon })
	const sessions = store.bucket<{ user: number }>('sessions')
	await sessions.set('token', { user: 42 })
	assert.deepEqual(await sessions.get('token'), { user: 42 })

	const counts = store.bucket<number>('counts')
	await Promise.all(Array.from({ length: 200 }, (_, n) => counts.set(`k${n}`, n)))
	const read = await Promise.all(Array.from({ length: 200 }, (_, n) => counts.get(`k${n}`)))
	assert.deepEqual(
		read,
		Array.from({ length: 200 }, (_, n) => n),
	)

	// past a frame and past the credit a stream starts with: pieces both ways
	const bytes = Uint8Array.from({ length: (3 << 20) + 17 }, (_, n) => n % 251)
	const files = store.files('pictures')
	await files.put('big.bin', bytes)
	assert.deepEqual(await (await files.get('big.bin'))?.bytes(), bytes)

	// what nobody asked for at that moment comes by the core's wake
	const chat = store.channel<string>('chat')
	const received: string[] = []
	const sub = await chat.subscribe(m => {
		received.push(m.value)
	})
	for (const text of ['a', 'b', 'c']) {
		await chat.publish(text)
	}
	await until(() => received.length === 3)
	assert.deepEqual(received, ['a', 'b', 'c'])
	await sub.stop()

	const emails = store.queue<{ to: string }>('emails')
	const sent: string[] = []
	const worker = emails.work(async ({ to }) => {
		sent.push(to)
	})
	await emails.add({ to: 'ada@example.com' })
	await until(() => sent.length === 1)
	await worker.stop()
	assert.deepEqual(sent, ['ada@example.com'])
	await store.close()

	// closed, the store lets go of its directory, and opens again with what it kept
	const again = await open(dir, { embedded: true, library: addon })
	assert.deepEqual(await again.bucket<{ user: number }>('sessions').get('token'), { user: 42 })
	await again.close()
	rmSync(dir, { recursive: true, force: true })
})

// contract: ERR-03, WIRE-01
test('a directory that is a file fails under Node, InvalidError naming it, before the core loads', {
	skip: addon === undefined && 'TINYSTORE_ADDON names no tinystore.node',
}, async () => {
	const file = join(scratch, 'a-file')
	writeFileSync(file, 'not a directory')
	await assert.rejects(open(file, { embedded: true, library: addon }), {
		name: 'InvalidError',
		message: `store ${file} is a file, not a directory`,
	})
})

test('a store opens through its sidecar under Node, and again through the one running', async () => {
	const dir = join(scratch, 'sidecar')
	const first = await open(dir, { idle: '2s' })
	await first.bucket<string>('notes').set('a', 'written under Node')
	const second = await open(dir)
	assert.equal(await second.bucket<string>('notes').get('a'), 'written under Node')
	await second.close()
	await first.close()
})

test('a private child serves its parent under Node, and frees its directory when closed', async () => {
	const dir = join(scratch, 'private')
	const store = await open(dir, { private: true })
	const videos = store.queue<{ video: number }>('videos')
	await videos.add({ video: 7 }, { id: 'v7' })
	const job = await videos.get('v7')
	assert.equal(job?.state, 'waiting')
	await store.close()
	rmSync(dir, { recursive: true, force: true }) // a child still running holds it on Windows
})

test('a remote server is reached over TCP with its token under Node', async () => {
	const { endpoint, token } = await serveRemote('tcp', [])
	const store = await connect(endpoint, { token })
	await store.bucket<number>('counts').set('n', 42)
	assert.equal(await store.bucket<number>('counts').get('n'), 42)
	await store.close()
	await assert.rejects(connect(endpoint, { token: 'not-the-token' }))
})

test('a remote server is reached over TLS, its certificate checked, under Node', async t => {
	const certificate = selfSigned()
	if (certificate === undefined) {
		t.skip('no openssl to make a certificate with')
		return
	}
	const { endpoint, token } = await serveRemote('tls', [
		'--tls-cert',
		certificate.cert,
		'--tls-key',
		certificate.key,
	])
	const ca = readFileSync(certificate.cert, 'utf8')
	for (const tls of [{ ca, serverName: 'localhost' }, { ca }]) {
		const store = await connect(endpoint, { token, tls })
		await store.bucket<string>('notes').set('t', 'over TLS')
		assert.equal(await store.bucket<string>('notes').get('t'), 'over TLS')
		await store.close()
	}
	await assert.rejects(connect(endpoint, { token }), 'a certificate nobody vouches for was trusted')
})

async function until(done: () => boolean): Promise<void> {
	for (let tried = 0; !done() && tried < 500; tried++) {
		await sleep(20)
	}
}

/** A server listening on a port of its choosing, its endpoint read from its SERVE. */
async function serveRemote(
	scheme: 'tcp' | 'tls',
	flags: string[],
): Promise<{ endpoint: string; token: string }> {
	const token = randomBytes(32).toString('base64url')
	const tokens = join(scratch, `${scheme}-tokens`)
	writeFileSync(
		tokens,
		`admin ${token}
`,
	)
	const dir = join(scratch, `${scheme}-store`)
	const server = spawn(
		process.env.TINYSTORE_BIN!,
		['serve', dir, '--listen', `${scheme}://127.0.0.1:0`, '--tokens', tokens, ...flags],
		{ stdio: 'ignore' },
	)
	servers.push(server)
	const serve = join(dir, 'server', 'SERVE')
	for (let tried = 0; !existsSync(serve); tried++) {
		assert.ok(tried < 500 && server.exitCode === null, `serve did not start: ${server.exitCode}`)
		await sleep(20)
	}
	const endpoints: string[] = JSON.parse(readFileSync(serve, 'utf8')).endpoints
	const endpoint = endpoints.find(at => at.startsWith(`${scheme}://`))
	assert.ok(endpoint !== undefined, `no ${scheme} endpoint in ${endpoints.join(', ')}`)
	return { endpoint, token }
}

/** A certificate for localhost and 127.0.0.1, and its key; undefined without openssl. */
function selfSigned(): { cert: string; key: string } | undefined {
	const cert = join(scratch, 'cert.pem')
	const key = join(scratch, 'key.pem')
	const made = spawnSync(
		'openssl',
		[
			'req',
			'-x509',
			'-newkey',
			'ec',
			'-pkeyopt',
			'ec_paramgen_curve:P-256',
			'-nodes',
			'-days',
			'1',
			'-subj',
			'/CN=localhost',
			'-addext',
			'subjectAltName=DNS:localhost,IP:127.0.0.1',
			'-keyout',
			key,
			'-out',
			cert,
		],
		{ encoding: 'utf8' },
	)
	return made.status === 0 ? { cert, key } : undefined
}
