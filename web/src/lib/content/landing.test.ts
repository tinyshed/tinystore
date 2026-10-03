import { describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { landingSummary, readCard, readCards, readLanding } from './landing'
import { checkout } from './repo'
import { loadSite } from './site'

describe('the landing page', () => {
	test("takes every word from web/landing.md, its links resolved as a page's", async () => {
		const landing = await readLanding(await loadSite())

		expect(landing.headline).not.toBe('')
		expect(landing.title).toBe(
			`TinyStore: ${landing.headline.charAt(0).toLowerCase()}${landing.headline.slice(1).replace(/\.$/, '')}`,
		)
		expect(landing.pitch).not.toBe('')
		expect(landing.pitch).not.toContain('<p>')
		expect(landing.sample).toContain('data-variant="go"')
		expect(landing.engines.length).toBeGreaterThan(0)
		for (const engine of landing.engines) {
			expect(engine.href).toMatch(engine.external ? /^https:\/\// : /^\/docs/)
		}
		expect(landing.numbers.legend.length).toBeGreaterThan(0)
		expect(landing.numbers.caveat).toContain('<a href="https://')
		expect(landingSummary()).toStartWith(`${landing.headline.replace(/\.$/, '')}: `)
	})

	test('fails the build when a part is missing or a link leads nowhere', async () => {
		const site = await loadSite()
		const root = mkdtempSync(join(tmpdir(), 'landing-'))
		const write = (text: string) => {
			mkdirSync(join(root, 'web'), { recursive: true })
			writeFileSync(join(root, 'web', 'landing.md'), text)
		}
		try {
			write('# Headline.\n\nThe pitch.\n\n```ts\nopen()\n```\n')
			await expect(readLanding(site, root)).rejects.toThrow(/## Engines/)

			write(
				[
					'# Headline.',
					'',
					'The pitch.',
					'',
					'```ts',
					'open()',
					'```',
					'',
					'## Engines',
					'',
					'| | | |',
					'|---|---|---|',
					'| [KV](../gone/README.md) | State | Current state. |',
					'',
					'## Numbers.',
					'',
					'| | |',
					'|---|---|',
					'| Batch | One file. |',
					'',
					'Measured somewhere.',
				].join('\n'),
			)
			await expect(readLanding(site, root)).rejects.toThrow(/gone\/README\.md does not exist/)
		} finally {
			rmSync(root, { recursive: true, force: true })
		}
	})
})

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
