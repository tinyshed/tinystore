import type { Element, ElementContent } from 'hast'
import { h, s } from 'hastscript'

/** One entry of a tree: its name, what the text says of it, and what it holds. */
export interface Entry {
	name: string
	note: string
	children: Entry[]
}

/**
 * A ```tree fence: a directory as GitHub shows any text, drawn with `├──`,
 * `└──` and `│`, or indented two spaces a level. A name ends at two spaces,
 * and what follows is its note; a name that ends with `/` is a folder.
 *
 * ```text
 * data/
 * ├── kv.db       buckets        data/ ─┬─ kv.db   buckets
 * └── sql/                              └─ sql/ ── chat.db   a database
 *     └── chat.db   a database
 * ```
 */
export function parseTree(text: string): Entry[] {
	const roots: Entry[] = []
	const open: { depth: number; entry: Entry }[] = []
	for (const line of text.split('\n')) {
		if (line.trim() === '') {
			continue
		}
		const { depth, rest } = depthOf(line)
		const [name = '', note = ''] = rest.split(/\s{2,}/, 2)
		const entry: Entry = { name: name.trim(), note: note.trim(), children: [] }
		while (open.length > 0 && (open.at(-1)?.depth ?? 0) >= depth) {
			open.pop()
		}
		const parent = open.at(-1)
		if (parent === undefined) {
			roots.push(entry)
		} else {
			parent.entry.children.push(entry)
		}
		open.push({ depth, entry })
	}
	return roots
}

/** A line's level, from its drawing (four characters a level) or its indent (two spaces). */
function depthOf(line: string): { depth: number; rest: string } {
	const drawn = line.match(/^((?:│ {3}| {4})*)(├── |└── )/)
	if (drawn !== null) {
		const prefix = drawn[0]
		return { depth: prefix.length / 4, rest: line.slice(prefix.length) }
	}
	const indent = line.length - line.trimStart().length
	return { depth: Math.floor(indent / 2), rest: line.trimStart() }
}

/** The tree as the page draws it: a list of lists, a folder's or a file's icon, the notes dimmed. */
export function renderTree(text: string): Element {
	return h('figure.tree', [list(parseTree(text))])
}

function list(entries: Entry[]): Element {
	return h(
		'ul',
		entries.map(entry =>
			h('li', [
				h('span.tree-entry', [
					icon(entry.name.endsWith('/')),
					h('span.tree-name', entry.name),
					...(entry.note === '' ? [] : [h('span.tree-note', note(entry.note))]),
				]),
				...(entry.children.length === 0 ? [] : [list(entry.children)]),
			]),
		),
	)
}

/** A note's text, its `code` drawn as code. */
function note(text: string): ElementContent[] {
	return text
		.split(/(`[^`]+`)/)
		.filter(part => part !== '')
		.map(part =>
			part.startsWith('`') ? h('code', part.slice(1, -1)) : { type: 'text', value: part },
		)
}

function icon(folder: boolean): Element {
	const path = folder
		? 'M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z'
		: 'M4 2.5A1.5 1.5 0 0 1 5.5 1H9l3.5 3.5v8A1.5 1.5 0 0 1 11 14H5.5A1.5 1.5 0 0 1 4 12.5zM9 1v3.5h3.5'
	return s(
		folder ? 'svg.tree-icon.folder' : 'svg.tree-icon',
		{
			viewBox: '0 0 16 16',
			width: 15,
			height: 15,
			fill: 'none',
			stroke: 'currentColor',
			strokeWidth: 1.3,
			ariaHidden: 'true',
		},
		[s('path', { d: path, strokeLinejoin: 'round' })],
	)
}
