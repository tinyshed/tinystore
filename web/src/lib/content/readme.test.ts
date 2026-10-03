import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { landingFile } from './landing'
import { readmeFile, readmeFromLanding, readmeProblems, readmes } from './readme'
import { checkout } from './repo'
import { loadSite } from './site'

const landing = [
	'# Headline.',
	'',
	'The pitch.',
	'',
	'```ts title="app.ts" install="bun add @tinyshed/tinystore"',
	"import { open } from '@tinyshed/tinystore'",
	'```',
	'',
	'```python install="pip install tinyshed-tinystore"',
	'import tinystore',
	'```',
	'',
	'```go install="go get github.com/tinyshed/tinystore"',
	'store, err := tinystore.Open(ctx, "./data", tinystore.Options{})',
	'```',
	'',
	'## Engines',
	'',
	'| | | |',
	'|---|---|---|',
	'| [KV](../docs/kv/README.md) | State | Current state. |',
	'',
].join('\n')

describe('the README', () => {
	test.each(readmes)('$file is what task readme writes into it from web/landing.md', shape => {
		const root = checkout()
		const readme = readFileSync(join(root, shape.file), 'utf8')

		expect(readmeFromLanding(readme, readFileSync(join(root, landingFile), 'utf8'), shape)).toBe(
			readme,
		)
	})

	test('centres the headline, shows the Go sample, folds the others and links from the top', () => {
		const readme = [
			'<!-- landing:headline -->',
			'<!-- /landing:headline -->',
			'',
			'<!-- landing:sample -->',
			'what was here before',
			'<!-- /landing:sample -->',
			'',
			'<!-- landing:engines -->',
			'<!-- /landing:engines -->',
			'',
		].join('\n')

		expect(readmeFromLanding(readme, landing)).toBe(
			[
				'<!-- landing:headline -->',
				'',
				'<p align="center">',
				'  <b>Headline.</b>',
				'</p>',
				'',
				'<p align="center">',
				'  The pitch.',
				'</p>',
				'',
				'<!-- /landing:headline -->',
				'',
				'<!-- landing:sample -->',
				'',
				'```go',
				'store, err := tinystore.Open(ctx, "./data", tinystore.Options{})',
				'```',
				'',
				'<details>',
				'<summary><b>Bun</b></summary>',
				'',
				'```ts',
				"import { open } from '@tinyshed/tinystore'",
				'```',
				'',
				'</details>',
				'',
				'<details>',
				'<summary><b>Python</b></summary>',
				'',
				'```python',
				'import tinystore',
				'```',
				'',
				'</details>',
				'',
				'```sh',
				'go get github.com/tinyshed/tinystore',
				'bun add @tinyshed/tinystore',
				'pip install tinyshed-tinystore',
				'```',
				'',
				'<!-- /landing:sample -->',
				'',
				'<!-- landing:engines -->',
				'',
				'| | | |',
				'|---|---|---|',
				'| [KV](docs/kv/README.md) | State | Current state. |',
				'',
				'<!-- /landing:engines -->',
				'',
			].join('\n'),
		)
	})

	test('shows a package its own language alone, its headline in markdown, and links to GitHub', () => {
		const readme = [
			'<!-- landing:headline -->',
			'<!-- /landing:headline -->',
			'<!-- landing:sample -->',
			'<!-- /landing:sample -->',
			'<!-- landing:engines -->',
			'<!-- /landing:engines -->',
		].join('\n')

		expect(
			readmeFromLanding(readme, landing, {
				file: 'sdk/js/README.md',
				languages: ['bun'],
				page: 'package',
			}),
		).toBe(
			[
				'<!-- landing:headline -->',
				'',
				'**Headline.** The pitch.',
				'',
				'<!-- /landing:headline -->',
				'<!-- landing:sample -->',
				'',
				'```ts',
				"import { open } from '@tinyshed/tinystore'",
				'```',
				'',
				'```sh',
				'bun add @tinyshed/tinystore',
				'```',
				'',
				'<!-- /landing:sample -->',
				'<!-- landing:engines -->',
				'',
				'| | | |',
				'|---|---|---|',
				'| [KV](https://github.com/tinyshed/tinystore/blob/main/docs/kv/README.md) | State | Current state. |',
				'',
				'<!-- /landing:engines -->',
			].join('\n'),
		)
	})

	test("refuses a README without a part's markers", () => {
		expect(() => readmeFromLanding('# TinyStore\n', landing)).toThrow('<!-- landing:headline -->')
	})

	test('has no link that leads nowhere', async () => {
		const site = await loadSite()
		const readme = readFileSync(join(checkout(), readmeFile), 'utf8')

		expect(readmeProblems(readme, site)).toEqual([])
		expect(readmeProblems('[gone](docs/gone.md), [kv](docs/kv/README.md#nowhere)', site)).toEqual([
			'docs/gone.md does not exist',
			'docs/kv/README.md has no heading #nowhere',
		])
	})

	test.each(readmes.filter(shape => shape.page === 'package'))(
		'$file links only to what exists, on GitHub',
		async shape => {
			const site = await loadSite()
			const readme = readFileSync(join(checkout(), shape.file), 'utf8')

			expect(readmeProblems(readme, site, checkout(), shape)).toEqual([])
			expect(
				readmeProblems(
					'[guides](../../docs/README.md), [gone](https://github.com/tinyshed/tinystore/blob/main/docs/gone.md)',
					site,
					checkout(),
					shape,
				),
			).toEqual([
				'../../docs/README.md is relative, which npm and PyPI cannot follow',
				'docs/gone.md does not exist',
			])
		},
	)
})
