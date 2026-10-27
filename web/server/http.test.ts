import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { logger } from '@tinyshed/tinystore'

import type { Analytics, ReaderEventInput } from './analytics.ts'
import { readSite, type SiteFile } from './files.ts'
import { createRouter, prefersMarkdown } from './http.ts'

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
		'404.md': '# Not found',
		'llms.txt': '# TinyStore',
		'sitemap.xml': '<urlset><url><loc>https://docs.example/</loc></url></urlset>',
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

describe('an agent', () => {
	const markdown = { accept: 'text/markdown, text/html, */*' }

	test('that asks for markdown is handed the page in it, at the same address', async () => {
		views.length = 0
		const response = await call('/docs/kv', { headers: markdown })

		expect(await response.text()).toBe('# KV')
		expect(response.headers.get('content-type')).toBe('text/markdown; charset=utf-8')
		expect(response.headers.get('vary')).toBe('accept-encoding, accept')
		expect(views).toEqual([{ page: '/docs/kv', kind: 'markdown', address: '10.0.0.1' }])

		const html = await call('/docs/kv')
		expect(await html.text()).toBe('kv')
		expect(html.headers.get('vary')).toBe('accept-encoding, accept')
	})

	test('asking for markdown at the landing page is handed llms.txt', async () => {
		expect(await (await call('/', { headers: markdown })).text()).toBe('# TinyStore')
	})

	test('finds where llms.txt is from any response, wherever the site is served', async () => {
		for (const path of ['/', '/docs/kv.md', '/favicon.svg', '/docs/nowhere', '/api/nope']) {
			const response = await call(path)
			expect(response.headers.get('link')).toBe(
				'<https://docs.example/llms.txt>; rel="llms-txt", <https://docs.example/llms-full.txt>; rel="llms-full-txt"',
			)
			expect(response.headers.get('x-llms-txt')).toBe('https://docs.example/llms.txt')
		}
	})

	test('that asks for markdown and finds nothing is told in markdown where the pages are', async () => {
		views.length = 0
		for (const [path, init] of [
			['/docs/nowhere.md', undefined],
			['/llms-cobol.txt', undefined],
			['/docs/nowhere', { headers: markdown }],
			['/404.md', undefined],
		] as const) {
			const response = await call(path, init)
			expect(response.status).toBe(404)
			expect(await response.text()).toBe('# Not found')
		}
		expect(await (await call('/docs/nowhere')).text()).toBe('missing')
		expect(views).toEqual([])
	})

	test('is told apart from a browser by what it accepts', () => {
		for (const accept of [
			'text/markdown',
			'text/markdown, text/html, */*',
			'text/html;q=0.9, text/markdown',
			'text/plain, */*',
			'text/plain, text/html;q=0',
		]) {
			expect(prefersMarkdown(accept)).toBe(true)
		}
		for (const accept of [
			null,
			'*/*',
			'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
			'text/html, text/plain',
			'text/html, text/markdown;q=0.5',
			'text/markdown;q=0, text/plain;q=0',
		]) {
			expect(prefersMarkdown(accept)).toBe(false)
		}
	})
})

describe('go get', () => {
	test('reads the Go module in sdk/go of the repository, at the root and below it', async () => {
		for (const path of ['/?go-get=1', '/kv?go-get=1', '/records/console?go-get=1']) {
			const response = await call(path)
			expect(response.status).toBe(200)
			expect(await response.text()).toContain(
				'<meta name="go-import" content="tinyshed.org/tinystore git https://github.com/tinyshed/tinystore sdk/go">',
			)
		}
	})

	test('counts no view', async () => {
		const before = views.length
		await call('/docs/kv?go-get=1')
		expect(views).toHaveLength(before)
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
