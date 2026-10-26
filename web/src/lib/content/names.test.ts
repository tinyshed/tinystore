import { describe, expect, test } from 'bun:test'
import { toHtml } from 'hast-util-to-html'

import { languageNames } from './names'

describe('a name in each language', () => {
	test('four spellings become one name, each spelling its language', () => {
		const names = languageNames('defaultTtl / default_ttl / DefaultTTL / default_ttl')

		expect(names).toBeDefined()
		const html = toHtml(names ?? [])
		expect(html).toContain('class="names"')
		expect(html).toContain('<span data-lang="bun">defaultTtl</span>')
		expect(html).toContain('<span data-lang="go">DefaultTTL</span>')
		expect(html).toContain(
			'title="Bun: defaultTtl · Python: default_ttl · Go: DefaultTTL · Rust: default_ttl"',
		)
	})

	test('a spelling may hold spaces, as a call does', () => {
		const html = toHtml(
			languageNames(
				'calendar: \'day\' / calendar="day" / Calendar: tinystore.Day / .calendar(Calendar::Day, zone)',
			) ?? [],
		)

		expect(html).toContain('<span data-lang="rust">.calendar(Calendar::Day, zone)</span>')
	})

	test('any other inline code stays as it is', () => {
		for (const plain of ['defaultTtl', 'a / b', 'a / b / c', 'a / b / c / d / e', 'a /  / c / d']) {
			expect(languageNames(plain)).toBeUndefined()
		}
	})
})
