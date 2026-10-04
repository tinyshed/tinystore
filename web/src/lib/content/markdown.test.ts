import { describe, expect, test } from 'bun:test'

import type { Highlight } from './highlight'
import type { Links } from './links'
import { exportMarkdown, finish, parse, toHast } from './markdown'
import { checkout } from './repo'

const plain: Highlight = code => ({
	type: 'element',
	tagName: 'pre',
	properties: {},
	children: [{ type: 'text', value: code }],
})

const links = (): Links => ({
	root: checkout(),
	commit: 'abc123',
	pages: new Map([['docs/kv.md', { url: '/docs/kv', ids: new Set(['expiry']) }]]),
	assets: new Set(),
})

const from = 'docs/page.md'

describe('a page as HTML', () => {
	test('serves a picture its repository files, each theme its own', async () => {
		const tree = parse(
			'<picture>\n<source media="(prefers-color-scheme: dark)" srcset="../.github/assets/logo-dark.svg">\n<img src="../.github/assets/logo-light.svg" alt="">\n</picture>\n',
		)
		const known = links()
		const { html, problems } = finish(await toHast(tree, plain), known, from, new Set())

		expect(problems).toEqual([])
		expect(html).toContain('srcset="/files/.github/assets/logo-dark.svg"')
		expect(html).toContain('src="/files/.github/assets/logo-light.svg"')
		expect([...known.assets].sort()).toEqual([
			'.github/assets/logo-dark.svg',
			'.github/assets/logo-light.svg',
		])
	})

	test('names every link that leads nowhere, with its line', async () => {
		const tree = parse('First.\n\n[gone](gone.md) and [no heading](kv.md#nowhere)\n')
		const { problems } = finish(await toHast(tree, plain), links(), from, new Set())

		expect(problems).toEqual([
			{ line: 3, message: 'docs/gone.md does not exist' },
			{ line: 3, message: 'docs/kv.md has no heading #nowhere' },
		])
	})

	test('draws a table with an empty header as rows alone', async () => {
		const { html } = finish(
			await toHast(parse('| | |\n|---|---|\n| a | `b` |\n'), plain),
			links(),
			from,
			new Set(),
		)

		expect(html).toContain('<div class="table"><table class="headless">')
	})

	test('resolves and checks the links inside a table', async () => {
		const { html, problems } = finish(
			await toHast(parse('| Page | Gone |\n|---|---|\n| [KV](kv.md) | [x](gone.md) |\n'), plain),
			links(),
			from,
			new Set(),
		)

		expect(html).toContain('href="/docs/kv"')
		expect(problems).toEqual([{ line: 3, message: 'docs/gone.md does not exist' }])
	})
})

describe('a page as markdown elsewhere', () => {
	test("links absolutely and leaves the editors' notes out", () => {
		const markdown = exportMarkdown(
			parse('# Page\n\n<!-- a note -->\n\n[KV](kv.md#expiry), [code](../kv/kv.go)\n'),
			links(),
			from,
			new Set(),
			'https://docs.example',
		)

		expect(markdown).toContain('[KV](https://docs.example/docs/kv#expiry)')
		expect(markdown).toContain('[code](https://github.com/tinyshed/tinystore/blob/abc123/kv/kv.go)')
		expect(markdown).not.toContain('a note')
	})
})
