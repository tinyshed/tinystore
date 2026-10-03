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
					'## Metrics',
					'- [Overview](metrics/README.md)',
					'- Instruments',
					'## Reference',
					'- [Bun API](../sdk/js/README.md)',
					'- [Elsewhere](https://example.com)',
				].join('\n'),
			),
			checkout(),
			'abc123',
		)

		expect(index.title).toBe('Docs')
		expect(index.sections.map(section => section.title)).toEqual(['Metrics', 'Reference'])
		expect(index.sections[0]?.items).toEqual([
			{ kind: 'page', title: 'Overview', file: 'docs/metrics/README.md', slug: 'metrics' },
			{ kind: 'planned', title: 'Instruments' },
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
		expect(slugOf('docs/kv/quotas.md')).toBe('kv/quotas')
		expect(slugOf('docs/kv/README.md')).toBe('kv')
		expect(slugOf('docs/README.md')).toBe('')
		expect(urlOf('')).toBe('/docs')
		expect(urlOf('kv/quotas')).toBe('/docs/kv/quotas')
	})
})
