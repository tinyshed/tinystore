import { describe, expect, test } from 'bun:test'
import type { Element, Root } from 'hast'
import { toHtml } from 'hast-util-to-html'
import { h } from 'hastscript'

import { type Drafted, markNew } from './news'

function page(file: string, url: string, title: string, ...children: Element[]): Drafted {
	return { file, url, title, hast: { type: 'root', children } as Root }
}

describe('what the newest release brought', () => {
	test("a heading the release links gets a badge back to the release's section", () => {
		const guide = page(
			'docs/guides/metrics/aggregates.md',
			'/docs/guides/metrics/aggregates',
			'Aggregates',
			h('h2', { id: 'days-in-a-time-zone' }, 'Days in a time zone'),
		)
		const release = page(
			'docs/releases/v0.2.0.md',
			'/docs/releases/v0.2.0',
			'TinyStore 0.2.0',
			h('h2', { id: 'days-in-a-time-zone' }, 'Days in a time zone'),
			h('p', [
				h('a', { href: '../guides/metrics/aggregates.md#days-in-a-time-zone' }, 'Aggregates'),
			]),
		)

		markNew([guide, release], release)

		const html = toHtml(guide.hast)
		expect(html).toContain('<a class="new-in" href="../../releases/v0.2.0.md#days-in-a-time-zone"')
		expect(html).toContain('>New in 0.2.0</a></h2>')
	})

	test('a page the release links whole shows the release by its title', () => {
		const guide = page(
			'docs/guides/jobs/concurrency.md',
			'/docs/guides/jobs/concurrency',
			'Concurrency and rate',
		)
		const release = page(
			'docs/releases/v0.2.0.md',
			'/docs/releases/v0.2.0',
			'TinyStore 0.2.0',
			h('h2', { id: 'a-rate-for-each-group-of-jobs' }, 'A rate for each group of jobs'),
			h('p', [h('a', { href: '../guides/jobs/concurrency.md' }, 'Concurrency and rate')]),
		)

		markNew([guide, release], release)

		expect(guide.newIn).toEqual({
			version: '0.2.0',
			url: '/docs/releases/v0.2.0#a-rate-for-each-group-of-jobs',
		})
	})

	test('links within the release, out of the site or to a heading twice add nothing more', () => {
		const guide = page(
			'docs/guides/kv/expiry.md',
			'/docs/guides/kv/expiry',
			'Expiry',
			h('h2', { id: 'ttl' }, 'TTL'),
		)
		const release = page(
			'docs/releases/v0.2.0.md',
			'/docs/releases/v0.2.0',
			'TinyStore 0.2.0',
			h('p', [
				h('a', { href: '#install' }, 'Install'),
				h('a', { href: 'https://github.com/tinyshed/tinystore' }, 'GitHub'),
				h('a', { href: '../guides/kv/expiry.md#ttl' }, 'one'),
				h('a', { href: '../guides/kv/expiry.md#ttl' }, 'two'),
			]),
		)

		markNew([guide, release], release)

		expect(toHtml(guide.hast).match(/new-in/g)?.length).toBe(1)
	})
})
