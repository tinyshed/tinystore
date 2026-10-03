import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { logger } from 'tinystore'

import type { Analytics, ReaderEventInput } from './analytics.ts'
import { readSite, type SiteFile } from './files.ts'
import { createRouter } from './http.ts'

let dir: string
const views: { page: string; kind: string; address: string }[] = []
const events: ReaderEventInput[] = []
let limited = false

const analytics: Analytics = {
	async view(_, file: SiteFile, address) {
		views.push({ page: file.page, kind: file.kind, address })
	},
	async event(_, event) {
		events.push(event)
		return limited ? 'limited' : 'kept'
	},
}

let ready = true
const store = {
	async status() {
		if (!ready) {
			throw new Error('down')
		}
		return {}
	},
	metrics: { failures: 0, lastFailure: undefined },
	loggers: { readers: { dropped: 2 } },
}

let route: ReturnType<typeof createRouter>

beforeAll(() => {
	dir = mkdtempSync(join(tmpdir(), 'site-'))
	for (const [path, text] of Object.entries({
		'index.html': 'home',
		'docs/kv.html': 'kv',
		'docs/kv/__data.json': '{}',
		'docs/kv.md': '# KV',
		'404.html': 'missing',
		'favicon.svg': '<svg/>',
	})) {
		mkdirSync(dirname(join(dir, path)), { recursive: true })
		writeFileSync(join(dir, path), text)
	}
	route = createRouter(readSite(dir), analytics, store, logger('test', { console: 'off' }))
})

afterAll(() => rmSync(dir, { recursive: true, force: true }))

const call = (path: string, init?: RequestInit) =>
	route(new Request(`http://site${path}`, init), '10.0.0.1')

describe('a page', () => {
	test('is served and counted, as are its data and its markdown; an asset is not counted', async () => {
		views.length = 0

		expect(await (await call('/docs/kv')).text()).toBe('kv')
		await call('/docs/kv/__data.json?x-sveltekit-invalidated=01')
		await call('/docs/kv.md')
		await call('/favicon.svg')

		expect(views).toEqual([
			{ page: '/docs/kv', kind: 'page', address: '10.0.0.1' },
			{ page: '/docs/kv', kind: 'data', address: '10.0.0.1' },
			{ page: '/docs/kv', kind: 'markdown', address: '10.0.0.1' },
		])
	})

	test('that does not exist is the 404 page, counted as nothing', async () => {
		views.length = 0
		const response = await call('/docs/nowhere')

		expect(response.status).toBe(404)
		expect(await response.text()).toBe('missing')
		expect((await call('/404')).status).toBe(404)
		expect(views).toEqual([])
	})

	test('has one address: a trailing slash is moved for good', async () => {
		const response = await call('/docs/kv/?a=1')

		expect(response.status).toBe(308)
		expect(response.headers.get('location')).toBe('http://site/docs/kv?a=1')
	})

	test('takes nothing but GET and HEAD, and HEAD is not counted', async () => {
		views.length = 0

		expect((await call('/docs/kv', { method: 'DELETE' })).status).toBe(405)
		expect((await call('/docs/kv', { method: 'HEAD' })).status).toBe(200)
		expect(views).toEqual([])
	})

	test('carries the headers that keep it from being sniffed or framed', async () => {
		const response = await call('/')

		expect(response.headers.get('x-content-type-options')).toBe('nosniff')
		expect(response.headers.get('x-frame-options')).toBe('DENY')
	})
})

describe('an event', () => {
	const send = (body: string) => call('/api/events', { method: 'POST', body })

	test('the site counts is kept, as a beacon sends it', async () => {
		events.length = 0
		const response = await send(
			JSON.stringify({ name: 'copy-code', page: '/docs/kv', value: 'go' }),
		)

		expect(response.status).toBe(204)
		expect(events).toEqual([{ name: 'copy-code', page: '/docs/kv', value: 'go' }])
	})

	test('it does not count, or that is not JSON, is refused', async () => {
		expect((await send(JSON.stringify({ name: 'mine', page: '/' }))).status).toBe(400)
		expect((await send(JSON.stringify({ name: 'search', page: 'docs' }))).status).toBe(400)
		expect((await send('{ not json')).status).toBe(400)
	})

	test("past a reader's rate is refused", async () => {
		limited = true
		expect((await send(JSON.stringify({ name: 'search', page: '/' }))).status).toBe(429)
		limited = false
	})
})

describe('health', () => {
	test('says the process is up, and readiness whether the store answers', async () => {
		expect(await (await call('/api/health')).json()).toMatchObject({
			status: 'ok',
			flushesFailed: 0,
			linesDropped: { readers: 2 },
		})
		expect((await call('/api/ready')).status).toBe(200)

		ready = false
		expect((await call('/api/ready')).status).toBe(503)
		ready = true
	})

	test('an unknown API route is the JSON 404, not the page', async () => {
		const response = await call('/api/nope')

		expect(response.status).toBe(404)
		expect(await response.json()).toMatchObject({ code: 'NOT_FOUND' })
	})
})
