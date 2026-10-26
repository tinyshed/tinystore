import { describe, expect, test } from 'bun:test'
import type { Element, Root } from 'hast'
import { toHtml } from 'hast-util-to-html'
import { h } from 'hastscript'

import { type Drafted, markNew } from './news'

function page(file: string, title: string, ...children: Element[]): Drafted {
	return { file, title, hast: { type: 'root', children } as Root }
}

const release = (...links: Element[]) =>
	page(
		'docs/releases/v0.2.0.md',
		'TinyStore 0.2.0',
		h('h2', { id: 'days-in-a-time-zone' }, 'Days in a time zone'),
		h('p', links),
	)

describe('what the newest release brought', () => {
	test("a heading the release links gets a badge back to the release's section", () => {
		const guide = page(
			'docs/guides/metrics/aggregates.md',
			'Aggregates',
			h('h2', { id: 'days-in-a-time-zone' }, 'Days in a time zone'),
		)
		const notes = release(
			h('a', { href: '../guides/metrics/aggregates.md#days-in-a-time-zone' }, 'Aggregates'),
		)

		markNew([guide, notes], notes)

		const html = toHtml(guide.hast)
		expect(html).toContain('<a class="new-in" href="../../releases/v0.2.0.md#days-in-a-time-zone"')
		expect(html).toContain('>New in 0.2.0</a></h2>')
	})

	test('a page the release links whole is marked nowhere: it was there before', () => {
		const guide = page(
			'docs/guides/jobs/concurrency.md',
			'Concurrency and rate',
			h('h2', { id: 'the-limits' }, 'The limits'),
		)
		const notes = release(h('a', { href: '../guides/jobs/concurrency.md' }, 'Concurrency and rate'))

		markNew([guide, notes], notes)

		expect(toHtml(guide.hast)).not.toContain('new-in')
	})

	test('links within the release, out of the site or to a heading twice add nothing more', () => {
		const guide = page('docs/guides/kv/expiry.md', 'Expiry', h('h2', { id: 'ttl' }, 'TTL'))
		const notes = release(
			h('a', { href: '#install' }, 'Install'),
			h('a', { href: 'https://github.com/tinyshed/tinystore' }, 'GitHub'),
			h('a', { href: '../guides/kv/expiry.md#ttl' }, 'one'),
			h('a', { href: '../guides/kv/expiry.md#ttl' }, 'two'),
		)

		markNew([guide, notes], notes)

		expect(toHtml(guide.hast).match(/new-in/g)?.length).toBe(1)
	})
})
