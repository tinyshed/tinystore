import { describe, expect, test } from 'bun:test'

import { parse } from './markdown'
import { readIndex, slugOf, urlOf } from './nav'
import { checkout } from './repo'

describe('the index', () => {
	test('is the sidebar: sections, pages, links out and pages not written yet', () => {
		const index = readIndex(
			parse(
				[
					'# Docs',
					'## Store',
					'- KV',
					'- [Metrics](store/metrics.md)',
					'## Reference',
					'- [Bun API](../sdk/js/README.md)',
					'- [Elsewhere](https://example.com)',
				].join('\n'),
			),
			checkout(),
			'abc123',
		)

		expect(index.title).toBe('Docs')
		expect(index.sections.map(section => section.title)).toEqual(['Store', 'Reference'])
		expect(index.sections[0]?.items).toEqual([
			{ kind: 'planned', title: 'KV' },
			{ kind: 'page', title: 'Metrics', file: 'docs/store/metrics.md', slug: 'store/metrics' },
		])
		expect(index.sections[1]?.items).toEqual([
			{
				kind: 'link',
				title: 'Bun API',
				href: 'https://github.com/tinyshed/tinystore/blob/abc123/sdk/js/README.md',
			},
			{ kind: 'link', title: 'Elsewhere', href: 'https://example.com' },
		])
	})

	test('refuses an entry that links to nothing', () => {
		expect(() => readIndex(parse('## A\n- [Gone](gone.md)'), checkout(), 'abc123')).toThrow(
			/gone\.md/,
		)
	})

	test('a file is a page at its path', () => {
		expect(slugOf('docs/store/metrics.md')).toBe('store/metrics')
		expect(slugOf('docs/store/README.md')).toBe('store')
		expect(slugOf('docs/README.md')).toBe('')
		expect(urlOf('')).toBe('/docs')
		expect(urlOf('store/metrics')).toBe('/docs/store/metrics')
	})
})
