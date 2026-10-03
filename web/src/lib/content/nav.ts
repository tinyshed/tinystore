import type { List, Root } from 'mdast'
import { toString as textOf } from 'mdast-util-to-string'

import { exists, resolveFrom, sourceUrl } from './repo'

/** An entry of the sidebar: a page, a link out, or a page the index names and nobody wrote yet. */
export type NavItem =
	| { kind: 'page'; title: string; file: string; slug: string }
	| { kind: 'link'; title: string; href: string }
	| { kind: 'planned'; title: string }

export interface NavSection {
	title: string
	items: NavItem[]
}

export interface Index {
	title: string
	sections: NavSection[]
}

/** The file whose headings and lists are the sidebar, and the page at /docs. */
export const indexFile = 'docs/README.md'

/**
 * Reads the index: each second-level heading is a section, each item of the
 * list under it an entry, in the order written. GitHub shows the same file as
 * the table of contents of docs/.
 *
 *     ## KV                         KV
 *     - [Overview](kv/README.md)    Overview, /docs/kv
 *     - [Quotas](kv/quotas.md)      Quotas, /docs/kv/quotas
 *     - Sessions                    Sessions, written later: shown, not linked
 *     - [Go API](https://…)         Go API, a link out
 */
export function readIndex(tree: Root, root: string, commit: string): Index {
	let title = ''
	const sections: NavSection[] = []

	for (const node of tree.children) {
		if (node.type === 'heading' && node.depth === 1) {
			title = textOf(node)
		} else if (node.type === 'heading' && node.depth === 2) {
			sections.push({ title: textOf(node), items: [] })
		} else if (node.type === 'list') {
			sections.at(-1)?.items.push(...items(node, root, commit))
		}
	}

	return { title, sections }
}

function items(list: List, root: string, commit: string): NavItem[] {
	return list.children.map(item => {
		const paragraph = item.children[0]
		const link = paragraph?.type === 'paragraph' ? paragraph.children[0] : undefined
		if (link?.type !== 'link') {
			return { kind: 'planned', title: textOf(item).trim() }
		}
		return entry(textOf(link), link.url, root, commit)
	})
}

function entry(title: string, url: string, root: string, commit: string): NavItem {
	if (/^[a-z][a-z\d+.-]*:/i.test(url)) {
		return { kind: 'link', title, href: url }
	}
	const file = resolveFrom(indexFile, decodeURIComponent(url))
	const kind = file === undefined ? undefined : exists(root, file)
	if (file === undefined || kind === undefined) {
		throw new Error(`${indexFile}: "${title}" links to ${url}, which does not exist`)
	}
	if (kind === 'file' && file.startsWith('docs/') && file.endsWith('.md')) {
		return { kind: 'page', title, file, slug: slugOf(file) }
	}
	return { kind: 'link', title, href: sourceUrl(file, commit, kind) }
}

/** `docs/kv/quotas.md` → `kv/quotas`, `docs/kv/README.md` → `kv`, `docs/README.md` → `` */
export function slugOf(file: string): string {
	return file
		.replace(/^docs\/?/, '')
		.replace(/(^|\/)README\.md$/, '')
		.replace(/\.md$/, '')
}

export function urlOf(slug: string): string {
	return slug === '' ? '/docs' : `/docs/${slug}`
}
