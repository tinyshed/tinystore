import { describe, expect, test } from 'bun:test'
import { toHtml } from 'hast-util-to-html'

import type { Highlight } from './highlight'
import { parse, toHast } from './markdown'

// a stand-in that keeps the code readable in the output
const plain: Highlight = code => ({
	type: 'element',
	tagName: 'pre',
	properties: {},
	children: [{ type: 'text', value: code }],
})

const page = `# A page

\`\`\`scene
code ts
  await sessions.set('k7q2', session)
end
actor app "Your app" at 10,50 icon=user
actor disk "kv.db" at 90,50 icon=database
link app disk

step "The app writes." line=1
  send app -> disk "set"
\`\`\`
`

describe('a scene fence', () => {
	test('becomes a figure the player mounts, its code and steps readable without it', async () => {
		const html = toHtml(await toHast(parse(page), plain))

		expect(html).toContain('<figure class="scene" data-scene="{')
		expect(html).toContain(
			'<div class="scene-code"><pre>await sessions.set(\'k7q2\', session)</pre></div>',
		)
		expect(html).toContain('<ol class="scene-steps"><li>The app writes.</li></ol>')
	})

	test('that does not parse is a problem of its page', async () => {
		const problems: string[] = []
		await toHast(parse(page.replace('icon=user', 'icon=rocket')), plain, problems)

		expect(problems).toEqual([expect.stringContaining('app wears icon rocket')])
	})
})
