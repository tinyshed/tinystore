import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fields, open } from 'tinystore'

import { createAnalytics, language, origin } from './analytics.ts'
import type { SiteFile } from './files.ts'
import { tinystoreBinary } from './testing.ts'

let dir: string

// a build of tinystore on a cold cache outlasts a hook's default five seconds
beforeAll(() => {
	tinystoreBinary()
	dir = mkdtempSync(join(tmpdir(), 'analytics-'))
}, 120_000)

afterAll(() => rmSync(dir, { recursive: true, force: true }))

const page: SiteFile = {
	path: '/dev/null',
	type: 'text/html',
	etag: '"1"',
	kind: 'page',
	page: '/docs/kv',
	immutable: false,
}

const reader = (headers: Record<string, string> = {}) =>
	new Request('http://docs.example/docs/kv', {
		headers: { 'user-agent': 'Mozilla/5.0 Safari', host: 'docs.example', ...headers },
	})

describe('the site in its own store', () => {
	test('keeps a view and an event as records and counts both, through a restart', async () => {
		const store = await open(dir, { private: true })
		const readers = store.records.logger('readers', { console: 'off' })
		const analytics = createAnalytics(store, readers, new Set(['/docs/kv']))

		await analytics.view(
			reader({ referer: 'https://www.google.com/search?q=kv' }),
			page,
			'10.0.0.1',
		)
		await analytics.view(
			reader({ referer: 'http://docs.example/docs' }),
			{ ...page, kind: 'data' },
			'10.0.0.1',
		)
		expect(
			await analytics.event(
				reader(),
				{ name: 'copy-code', page: '/docs/kv', value: 'go' },
				'10.0.0.1',
			),
		).toBe('kept')
		expect(
			await analytics.event(reader(), { name: 'search', page: '/elsewhere' }, '10.0.0.1'),
		).toBe('kept')
		await store.close()

		const again = await open(dir, { private: true })
		try {
			const records = []
			for await (const record of again.records.all({ since: '1h' })) {
				records.push(record)
			}
			const kept = records.map(record => ({ name: record.name, ...fields(record.attrs) }))
			expect(kept).toContainEqual(
				expect.objectContaining({
					name: 'view',
					page: '/docs/kv',
					via: 'page',
					agent: 'person',
					referrer: 'www.google.com',
				}),
			)
			expect(kept).toContainEqual(
				expect.objectContaining({ name: 'view', via: 'data', from: '/docs' }),
			)
			expect(kept).toContainEqual(
				expect.objectContaining({ name: 'copy-code', page: '/docs/kv', value: 'go' }),
			)
			expect(kept).toContainEqual(expect.objectContaining({ name: 'search', page: 'other' }))

			const visitors = new Set(records.map(record => fields(record.attrs).visitor))
			expect(visitors.size).toBe(1)

			const views = await again.metrics.read({ name: 'site_views_total', since: '1h' })
			expect(
				views
					.map(series => series.labels)
					.sort((a, b) => String(a.via).localeCompare(String(b.via))),
			).toEqual([
				{ agent: 'person', page: '/docs/kv', via: 'data' },
				{ agent: 'person', page: '/docs/kv', via: 'page' },
			])
		} finally {
			await again.close()
		}
	})
})

describe('a reader', () => {
	test('came from a page of the site or from another site, never with its whole address', () => {
		expect(origin(reader({ referer: 'http://docs.example/docs/jobs?token=x' }))).toEqual({
			from: '/docs/jobs',
		})
		expect(origin(reader({ referer: 'https://news.ycombinator.com/item?id=1' }))).toEqual({
			referrer: 'news.ycombinator.com',
		})
		expect(origin(reader())).toEqual({})
	})

	test('reads in their first language', () => {
		expect(language(reader({ 'accept-language': 'ru-RU,ru;q=0.9,en;q=0.8' }))).toEqual({
			language: 'ru',
		})
		expect(language(reader({ 'accept-language': '*' }))).toEqual({})
		expect(language(reader())).toEqual({})
	})
})
