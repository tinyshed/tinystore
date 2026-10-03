import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { landingFile } from './landing'
import { readmeFile, readmeFromLanding, readmeProblems } from './readme'
import { checkout } from './repo'
import { loadSite } from './site'

const landing = [
	'# Headline.',
	'',
	'The pitch.',
	'',
	'```ts title="app.ts" install="bun add tinystore"',
	"import { open } from 'tinystore'",
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
	test('is what task readme writes into it from web/landing.md', () => {
		const root = checkout()
		const readme = readFileSync(join(root, readmeFile), 'utf8')

		expect(readmeFromLanding(readme, readFileSync(join(root, landingFile), 'utf8'))).toBe(readme)
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
				"import { open } from 'tinystore'",
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
				'bun add tinystore',
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
})
