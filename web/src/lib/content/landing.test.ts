import { describe, expect, test } from 'bun:test'

import { readCard, readCards } from './landing'
import { checkout } from './repo'

describe('the benchmark cards', () => {
	test('read back from every SVG the README shows, in its order', () => {
		const cards = readCards(checkout())

		expect(cards.length).toBeGreaterThan(0)
		for (const card of cards) {
			expect(card.title).not.toBe('')
			expect(card.rows.filter(row => row.ours)).toHaveLength(1)
			expect(Math.max(...card.rows.flatMap(row => row.bars.map(bar => bar.fraction)))).toBe(1)
		}
	})

	test('keep the idle and the load peak of a memory card apart', () => {
		const card = readCard(
			[
				'<svg><rect x="0.5" fill="none"/>',
				'<text x="20" y="36" font-size="16">An application, memory</text>',
				'<text x="20" y="56" font-size="12">Idle / load peak, MiB</text>',
				'<text x="20" y="87" font-size="14">TinyStore</text>',
				'<rect width="40" fill="#ECEAF3" opacity="1"/><text x="200" y="85">41.0</text>',
				'<rect width="120" fill="#ECEAF3" opacity="0.4"/><text x="300" y="105">111</text>',
				'<text x="20" y="135" font-size="14">Services</text>',
				'<rect width="80" fill="#3d444d" opacity="1"/><text x="230" y="133">75.3</text>',
				'</svg>',
			].join('\n'),
		)

		expect(card.title).toBe('An application, memory')
		expect(card.rows[0]).toEqual({
			label: 'TinyStore',
			ours: true,
			bars: [
				{ value: '41.0', fraction: 40 / 120, faded: false },
				{ value: '111', fraction: 1, faded: true },
			],
		})
		expect(card.rows[1]?.ours).toBe(false)
	})

	test('refuse an SVG that is no card', () => {
		expect(() => readCard('<svg><text x="20" y="36" font-size="16">Title</text></svg>')).toThrow(
			/no benchmark card/,
		)
	})
})
