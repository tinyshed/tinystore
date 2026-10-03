import { describe, expect, test } from 'bun:test'

import { type Entry, find, snippet } from './search'

const entry = (heading: string, text: string, kind: Entry['kind'] = 'guide', code = ''): Entry => ({
	page: 'Metrics',
	heading,
	url: `/docs/store/metrics#${heading}`,
	kind,
	text,
	code,
})

const entries = [
	entry('Aggregate exactly', 'increase is for counters'),
	entry('Count something', 'a reset, and increase accounts for it'),
	entry('Read it back', 'exact samples', 'design', 'store.metrics.read({ since })'),
]

describe('search', () => {
	test('finds the entries holding every word, a heading first', () => {
		const hits = find(entries, 'increase', 'all')
		expect(hits.map(hit => hit.entry.heading)).toEqual(['Aggregate exactly', 'Count something'])

		expect(find(entries, 'increase counters', 'all').map(hit => hit.entry.heading)).toEqual([
			'Aggregate exactly',
		])
		expect(find(entries, 'aggregate', 'all')[0]?.entry.heading).toBe('Aggregate exactly')
	})

	test('filters by kind, and Code looks in code alone', () => {
		expect(find(entries, 'exact', 'design').map(hit => hit.entry.heading)).toEqual(['Read it back'])
		expect(find(entries, 'since', 'code').map(hit => hit.entry.heading)).toEqual(['Read it back'])
		expect(find(entries, 'increase', 'code')).toEqual([])
	})

	test('marks every match and escapes the rest', () => {
		expect(snippet('a <b> and increase, Increase', ['increase'])).toBe(
			'a &#60;b&#62; and <mark>increase</mark>, <mark>Increase</mark>',
		)
	})

	test('an empty query finds nothing', () => {
		expect(find(entries, '   ', 'all')).toEqual([])
	})
})
