import { describe, expect, test } from 'bun:test'
import { toHtml } from 'hast-util-to-html'
import remarkRehype from 'remark-rehype'
import { unified } from 'unified'

import { calloutsFromAlerts } from './alerts'
import { parse } from './markdown'

async function render(markdown: string): Promise<string> {
	const tree = parse(markdown)
	calloutsFromAlerts(tree)
	return toHtml(await unified().use(remarkRehype).run(tree))
}

describe('alerts', () => {
	test('a bold first line names the callout', async () => {
		const html = await render('> [!NOTE]\n> **Counters**\n> `increase` is for counters.')

		expect(html).toContain('<aside class="callout callout-note">')
		expect(html).toContain('<p class="callout-label">Counters</p>')
		expect(html).toContain('<code>increase</code> is for counters.')
		expect(html).not.toContain('[!NOTE]')
	})

	test('without a bold line the kind is the label', async () => {
		const html = await render('> [!WARNING]\n> Back up first.')

		expect(html).toContain('<p class="callout-label">warning</p>')
		expect(html).toContain('Back up first.')
	})

	test('a quote and an unknown kind stay quotes', async () => {
		expect(await render('> Plain words.')).toContain('<blockquote>')
		expect(await render('> [!SHOUT]\n> Loud.')).toContain('<blockquote>')
	})
})
