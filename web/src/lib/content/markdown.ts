import type { Element, ElementContent, Root as HastRoot } from 'hast'
import { toHtml } from 'hast-util-to-html'
import { h } from 'hastscript'
import type { Root } from 'mdast'
import rehypeRaw from 'rehype-raw'
import remarkGfm from 'remark-gfm'
import remarkParse from 'remark-parse'
import remarkRehype from 'remark-rehype'
import remarkStringify from 'remark-stringify'
import { unified } from 'unified'
import { SKIP, visit } from 'unist-util-visit'

import { type CodeGroup, renderGroup } from './code'
import type { Highlight } from './highlight'
import { type Links, resolveLink } from './links'

const parser = unified().use(remarkParse).use(remarkGfm)

export function parse(text: string): Root {
	return parser.parse(text)
}

/** The tree as HTML elements, the raw HTML a file holds parsed into them. */
export async function toHast(tree: Root, highlight: Highlight): Promise<HastRoot> {
	const processor = unified()
		.use(remarkRehype, {
			allowDangerousHtml: true,
			handlers: { codeGroup: (_, node: CodeGroup) => renderGroup(node, highlight) },
		})
		.use(rehypeRaw)

	return (await processor.run(structuredClone(tree))) as HastRoot
}

export interface Problem {
	line?: number
	message: string
}

/**
 * The page's HTML, every link resolved for the site. Each link that leads
 * nowhere is a problem, so that one build reports all of them.
 */
export function finish(
	tree: HastRoot,
	links: Links,
	file: string,
	ids: Set<string>,
): { html: string; problems: Problem[] } {
	const problems: Problem[] = []

	visit(tree, 'element', (node, index, parent) => {
		for (const [property, as] of [
			['href', 'link'],
			['src', 'asset'],
		] as const) {
			const value = node.properties[property]
			if (typeof value !== 'string') {
				continue
			}
			const resolved = resolveLink(links, file, value, ids, node.tagName === 'a' ? 'link' : as)
			if ('problem' in resolved) {
				problems.push({ ...lineOf(node), message: resolved.problem })
			} else {
				node.properties[property] = resolved.href
			}
		}

		// a list in some trees and the attribute's text in others: `a.svg 1x, b.svg 2x`
		const srcset = node.properties.srcSet
		if (typeof srcset === 'string' || Array.isArray(srcset)) {
			const listed = typeof srcset === 'string' ? srcset.split(',') : srcset
			const candidates: string[] = listed.map(candidate => {
				const [url = '', ...descriptors] = String(candidate).trim().split(/\s+/)
				const resolved = resolveLink(links, file, url, ids, 'asset')
				if ('problem' in resolved) {
					problems.push({ ...lineOf(node), message: resolved.problem })
					return String(candidate)
				}
				return [resolved.href, ...descriptors].join(' ')
			})
			node.properties.srcSet = candidates.join(', ')
		}

		if (node.tagName === 'img') {
			node.properties.loading = 'lazy'
		}
		if (/^h[23]$/.test(node.tagName) && typeof node.properties.id === 'string') {
			node.children.unshift(anchor(node.properties.id))
		}
		if (node.tagName === 'table' && parent !== undefined && index !== undefined) {
			if (headless(node)) {
				node.properties.className = ['headless']
			}
			parent.children[index] = h('div.table', [node])
			return SKIP
		}
		return undefined
	})

	return { html: toHtml(tree), problems }
}

const exporter = unified().use(remarkGfm).use(remarkStringify, {
	bullet: '-',
	fences: true,
	rule: '-',
})

/**
 * The page's markdown for a reader elsewhere, an agent or a paste: links made
 * absolute so that they still lead somewhere, and the comments the site reads
 * left out.
 */
export function exportMarkdown(
	tree: Root,
	links: Links,
	file: string,
	ids: Set<string>,
	origin: string,
): string {
	const copy = structuredClone(tree)

	visit(copy, (node, index, parent) => {
		if (node.type === 'html' && /^<!--[\s\S]*-->$/.test(node.value.trim())) {
			parent?.children.splice(index ?? 0, 1)
			return [SKIP, index ?? 0]
		}
		if (node.type === 'link' || node.type === 'image' || node.type === 'definition') {
			const resolved = resolveLink(
				links,
				file,
				node.url,
				ids,
				node.type === 'image' ? 'asset' : 'link',
			)
			if ('href' in resolved) {
				node.url = resolved.href.startsWith('/') ? origin + resolved.href : resolved.href
			}
		}
		return undefined
	})

	return exporter.stringify(copy)
}

// GitHub's tables need a header row, so a table without one writes it empty: | | |
function headless(table: Element): boolean {
	const head = table.children.find(
		(child): child is Element => child.type === 'element' && child.tagName === 'thead',
	)
	if (head === undefined) {
		return false
	}
	const text = toHtml(head).replace(/<[^>]*>/g, '')
	return text.trim() === ''
}

function anchor(id: string): ElementContent {
	return h('a.anchor', { href: `#${id}`, ariaHidden: 'true', tabIndex: -1 }, '#')
}

function lineOf(node: Element): { line?: number } {
	const line = node.position?.start.line
	return line === undefined ? {} : { line }
}
