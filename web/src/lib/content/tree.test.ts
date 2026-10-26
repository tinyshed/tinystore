import { describe, expect, test } from 'bun:test'
import { toHtml } from 'hast-util-to-html'

import { parseTree, renderTree } from './tree'

describe('a tree fence', () => {
	test('reads a drawn tree into folders, files and their notes', () => {
		const tree = parseTree(`data/
├── kv.db             buckets, \`once\`
├── sql/
│   └── chat.db       a database
└── blobs/            uploaded files`)

		expect(tree).toEqual([
			{
				name: 'data/',
				note: '',
				children: [
					{ name: 'kv.db', note: 'buckets, `once`', children: [] },
					{
						name: 'sql/',
						note: '',
						children: [{ name: 'chat.db', note: 'a database', children: [] }],
					},
					{ name: 'blobs/', note: 'uploaded files', children: [] },
				],
			},
		])
	})

	test('reads a tree indented two spaces a level the same way', () => {
		const tree = parseTree('migrations/\n  0001_users.sql\n  0002_orders.sql')

		expect(tree[0]?.children.map(entry => entry.name)).toEqual([
			'0001_users.sql',
			'0002_orders.sql',
		])
	})

	test('draws folders and files apart, a note its code as code', () => {
		const html = toHtml(renderTree('data/\n└── kv.db   buckets, `once`'))

		expect(html).toContain('<figure class="tree">')
		expect(html).toContain('class="tree-icon folder"')
		expect(html).toContain('<span class="tree-name">kv.db</span>')
		expect(html).toContain('<span class="tree-note">buckets, <code>once</code></span>')
	})
})

describe('a tree as the page draws it', () => {
	test('lists folders before files, each kind in its own order', () => {
		const html = toHtml(renderTree('data/\n├── LOCK\n├── sql/\n├── kv.db\n└── blobs/'))
		const names = [...html.matchAll(/<span class="tree-name">([^<]+)<\/span>/g)].map(
			found => found[1],
		)

		expect(names).toEqual(['data/', 'sql/', 'blobs/', 'LOCK', 'kv.db'])
	})
})
