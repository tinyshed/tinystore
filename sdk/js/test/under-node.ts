// The SDK under Node, through Node's runtime, against a real tinystore: the
// sidecar, a private child, and a remote server over TCP and TLS. Node runs
// the TypeScript as it is: node --test test/under-node.ts.

import assert from 'node:assert/strict'
import { type ChildProcess, spawn, spawnSync } from 'node:child_process'
import { randomBytes } from 'node:crypto'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { after, before, test } from 'node:test'
import { setTimeout as sleep } from 'node:timers/promises'

import { connect, open } from '../src/index.ts'
import { nodeRuntime } from '../src/runtime/node.ts'

const scratch = mkdtempSync(join(tmpdir(), 'tinystore-node-'))
const servers: ChildProcess[] = []

before(() => {
	if (process.env.TINYSTORE_BIN !== undefined) {
		return
	}
	const out = join(import.meta.dirname, '..', '.bin')
	mkdirSync(out, { recursive: true })
	const binary = join(out, process.platform === 'win32' ? 'tinystore.exe' : 'tinystore')
	const built = spawnSync('go', ['build', '-o', binary, '.'], {
		cwd: resolve(import.meta.dirname, '../../../cmd/tinystore'),
		env: { ...process.env, GOWORK: 'off', CGO_ENABLED: '0' },
		encoding: 'utf8',
	})
	assert.equal(built.status, 0, `go build of tinystore failed: ${built.stderr}`)
	process.env.TINYSTORE_BIN = binary
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

test('a store opens through its sidecar under Node, and again through the one running', async () => {
	const dir = join(scratch, 'sidecar')
	const first = await open(dir, { idle: '2s' })
	await first.kv.bucket<string>('notes').set('a', 'written under Node')
	const second = await open(dir)
	assert.equal(await second.kv.bucket<string>('notes').get('a'), 'written under Node')
	await second.close()
	await first.close()
})

test('a private child serves its parent under Node, and frees its directory when closed', async () => {
	const dir = join(scratch, 'private')
	const store = await open(dir, { private: true })
	const videos = store.jobs.queue<{ video: number }>('videos')
	await videos.enqueue({ video: 7 }, { key: 'v7' })
	const job = await videos.get('v7')
	assert.equal(job?.state, 'waiting')
	await store.close()
	rmSync(dir, { recursive: true, force: true }) // a child still running holds it on Windows
})

test('a remote server is reached over TCP with its token under Node', async () => {
	const { endpoint, token } = await serveRemote('tcp', [])
	const store = await connect(endpoint, { token })
	await store.kv.bucket<number>('counts').set('n', 42)
	assert.equal(await store.kv.bucket<number>('counts').get('n'), 42)
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
		await store.kv.bucket<string>('notes').set('t', 'over TLS')
		assert.equal(await store.kv.bucket<string>('notes').get('t'), 'over TLS')
		await store.close()
	}
	await assert.rejects(connect(endpoint, { token }), 'a certificate nobody vouches for was trusted')
})

/** A server listening on a port of its choosing, its endpoint read from its log. */
async function serveRemote(
	scheme: 'tcp' | 'tls',
	flags: string[],
): Promise<{ endpoint: string; token: string }> {
	const token = randomBytes(32).toString('base64url')
	const tokens = join(scratch, `${scheme}-tokens`)
	writeFileSync(tokens, `admin ${token}\n`)
	const dir = join(scratch, `${scheme}-store`)
	const server = spawn(
		process.env.TINYSTORE_BIN!,
		['serve', '--dir', dir, '--listen', `${scheme}://127.0.0.1:0`, '--tokens', tokens, ...flags],
		{ stdio: ['ignore', 'ignore', 'pipe'] },
	)
	servers.push(server)
	let log = ''
	server.stderr!.setEncoding('utf8')
	const endpoint = await new Promise<string>((found, failed) => {
		server.stderr!.on('data', (text: string) => {
			log += text
			const serving = new RegExp(`endpoints=(${scheme}://\\S+)`).exec(log)
			if (serving !== null) {
				found(serving[1]!)
			}
		})
		server.once('exit', code => failed(new Error(`serve exited with ${code}: ${log}`)))
	})
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
