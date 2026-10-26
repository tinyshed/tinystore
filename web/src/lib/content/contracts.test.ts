import { describe, expect, test } from 'bun:test'
import { toHtml } from 'hast-util-to-html'
import remarkRehype from 'remark-rehype'
import { unified } from 'unified'

import { linkContracts } from './contracts'
import { parse } from './markdown'

const contracts = new Map([
	['STORE-05', "The store's memory MUST stay within the budget its program gives."],
])

async function render(markdown: string): Promise<string> {
	const tree = parse(markdown)
	linkContracts(tree, contracts, 'docs/handbook/inside/memory.md')
	return toHtml(await unified().use(remarkRehype).run(tree))
}

describe('contracts in the prose', () => {
	test("an id becomes a link to CONTRACTS.md carrying the contract's text", async () => {
		const html = await render('Waiters are served in order (contract STORE-05).')

		expect(html).toContain('(contract <a href="../../../CONTRACTS.md" class="contract"')
		expect(html).toContain(
			'data-contract="The store&#x27;s memory MUST stay within the budget its program gives."',
		)
		expect(html).toContain('>STORE-05</a>).')
	})

	test('an id in code, in a link or in a heading, or one no contract has, stays text', async () => {
		const html = await render('## STORE-05\n\n`STORE-05`, [STORE-05](x.md) and KV-99.')

		expect(html).not.toContain('class="contract"')
	})
})
