import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'

import { readSite, respond, routeOf, type Site } from './files.ts'

let dir: string
let site: Site

beforeAll(() => {
	dir = mkdtempSync(join(tmpdir(), 'site-'))
	const files: Record<string, string> = {
		'index.html': '<h1>home</h1>',
		'index.html.br': 'brotli bytes',
		'index.html.gz': 'gzip bytes',
		'docs.html': '<h1>docs</h1>',
		'docs/kv.html': '<h1>kv</h1>',
		'docs/kv/__data.json': '{}',
		'docs/kv.md': '# KV',
		'llms.txt': '# TinyStore',
		'404.html': '<h1>404</h1>',
		'_app/immutable/entry/start.js': 'export {}',
		'favicon.svg': '<svg/>',
	}
	for (const [path, text] of Object.entries(files)) {
		mkdirSync(dirname(join(dir, path)), { recursive: true })
		writeFileSync(join(dir, path), text)
	}
	site = readSite(dir)
})

afterAll(() => rmSync(dir, { recursive: true, force: true }))

describe('the build', () => {
	test('answers each path with its file, and knows which are pages', () => {
		expect(routeOf('index.html')).toEqual({ path: '/', kind: 'page', page: '/' })
		expect(routeOf('docs/kv.html')).toEqual({ path: '/docs/kv', kind: 'page', page: '/docs/kv' })
		expect(routeOf('docs/kv/__data.json')).toEqual({
			path: '/docs/kv/__data.json',
			kind: 'data',
			page: '/docs/kv',
		})
		expect(routeOf('__data.json')).toEqual({ path: '/__data.json', kind: 'data', page: '/' })
		expect(routeOf('docs/kv.md')).toEqual({
			path: '/docs/kv.md',
			kind: 'markdown',
			page: '/docs/kv',
		})
		expect(routeOf('llms.txt')).toEqual({ path: '/llms.txt', kind: 'markdown', page: '/llms.txt' })
		expect(routeOf('favicon.svg')).toEqual({ path: '/favicon.svg', kind: 'asset', page: '' })

		expect([...site.pages].sort()).toEqual(['/', '/404', '/docs', '/docs/kv'])
		expect(site.find('/index.html.br')).toBeUndefined()
	})

	test('refuses a directory with no index.html', () => {
		const empty = mkdtempSync(join(tmpdir(), 'empty-'))
		expect(() => readSite(empty)).toThrow(/bun run build/)
		rmSync(empty, { recursive: true })
	})
})

describe('a response', () => {
	const get = (path: string, headers: Record<string, string> = {}) => {
		const file = site.find(path)
		if (file === undefined) {
			throw new Error(`no ${path}`)
		}
		return respond(file, new Request(`http://site${path}`, { headers }))
	}

	test('is brotli when the reader takes it, gzip when only that, and the file otherwise', async () => {
		const br = get('/', { 'accept-encoding': 'gzip, deflate, br' })
		expect(br.headers.get('content-encoding')).toBe('br')
		expect(await br.text()).toBe('brotli bytes')

		const gzip = get('/', { 'accept-encoding': 'gzip' })
		expect(gzip.headers.get('content-encoding')).toBe('gzip')

		const plain = get('/')
		expect(plain.headers.get('content-encoding')).toBeNull()
		expect(await plain.text()).toBe('<h1>home</h1>')
		expect(plain.headers.get('vary')).toBe('accept-encoding')
	})

	test('is a 304 while the reader holds the same bytes, an encoding apart from another', () => {
		const first = get('/', { 'accept-encoding': 'br' })
		const etag = first.headers.get('etag') ?? ''
		expect(etag).toEndWith('-br"')

		expect(get('/', { 'accept-encoding': 'br', 'if-none-match': etag }).status).toBe(304)
		expect(get('/', { 'if-none-match': etag }).status).toBe(200)
	})

	test('keeps a hashed asset for good and asks again for a page', () => {
		expect(get('/_app/immutable/entry/start.js').headers.get('cache-control')).toContain(
			'immutable',
		)
		expect(get('/docs/kv').headers.get('cache-control')).toBe('no-cache')
		expect(get('/docs/kv.md').headers.get('content-type')).toBe('text/markdown; charset=utf-8')
	})
})
