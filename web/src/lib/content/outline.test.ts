import { describe, expect, test } from 'bun:test'

import { parse } from './markdown'
import { outline, sections } from './outline'

const page = [
	'# Metrics',
	'',
	'Exact samples.',
	'',
	'## Read it back',
	'',
	'Text.',
	'',
	'```ts',
	'read()',
	'```',
	'',
	'## Read it back',
	'',
	'### `Open` and `Close`',
].join('\n')

describe('the outline', () => {
	test('gives headings the ids GitHub gives them, a repeated one numbered', () => {
		const found = outline(parse(page))

		expect(found.headings).toEqual([
			{ id: 'read-it-back', text: 'Read it back' },
			{ id: 'read-it-back-1', text: 'Read it back' },
		])
		expect([...found.ids]).toContain('open-and-close')
	})

	test('takes the title out of the page and marks the lead', () => {
		const tree = parse(page)
		const found = outline(tree)

		expect(found.title).toBe('Metrics')
		expect(found.lead).toBe('Exact samples.')
		expect(tree.children[0]?.type).toBe('paragraph')
		expect(tree.children[0]?.data?.hProperties?.className).toEqual(['lead'])
	})

	test('cuts the page at its headings, the prose apart from the code', () => {
		const tree = parse(page)
		outline(tree)

		expect(sections(tree)).toEqual([
			{ heading: '', text: 'Exact samples.', code: '' },
			{ heading: 'Read it back', id: 'read-it-back', text: 'Text.', code: 'read()' },
			{ heading: 'Read it back', id: 'read-it-back-1', text: '', code: '' },
			{ heading: 'Open and Close', id: 'open-and-close', text: '', code: '' },
		])
	})
})
