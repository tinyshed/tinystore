import { readFileSync } from 'node:fs'
import { join, posix } from 'node:path'
import type { Link, Parent, PhrasingContent, Root, Text } from 'mdast'
import { SKIP, visit } from 'unist-util-visit'

/** What TinyStore promises, one contract a line: `- **STORE-05** The store's memory MUST …`. */
export const contractsFile = 'CONTRACTS.md'

/** Each contract's text by its id, as CONTRACTS.md says it, its markdown taken off. */
export function readContracts(root: string): Map<string, string> {
	const contracts = new Map<string, string>()
	for (const line of readFileSync(join(root, contractsFile), 'utf8').split('\n')) {
		const found = line.match(/^- \*\*([A-Z]+-\d+)\*\* (.+)$/)
		if (found?.[1] !== undefined && found[2] !== undefined) {
			contracts.set(found[1], found[2].replace(/\*\*|`/g, '').trim())
		}
	}
	return contracts
}

/**
 * Makes each contract's id in the page's prose, STORE-05, a link to
 * CONTRACTS.md that carries the contract's text, which the stylesheet shows
 * on a hover. An id in code, a link or a heading stays as it is, and so
 * does one CONTRACTS.md lacks.
 */
export function linkContracts(tree: Root, contracts: Map<string, string>, file: string): void {
	const to = posix.relative(posix.dirname(file), contractsFile)
	visit(tree, (node, index, parent) => {
		if (
			node.type === 'link' ||
			node.type === 'heading' ||
			node.type === 'inlineCode' ||
			node.type === 'code'
		) {
			return SKIP
		}
		if (node.type !== 'text' || parent === undefined || index === undefined) {
			return
		}
		const parts = split(node, contracts, to)
		if (parts.length === 1) {
			return
		}
		;(parent as Parent).children.splice(index, 1, ...parts)
		return [SKIP, index + parts.length]
	})
}

function split(text: Text, contracts: Map<string, string>, to: string): PhrasingContent[] {
	const parts: PhrasingContent[] = []
	let at = 0
	for (const found of text.value.matchAll(/\b[A-Z]{2,6}-\d{2}\b/g)) {
		const id = found[0]
		const said = contracts.get(id)
		if (said === undefined) {
			continue
		}
		if (found.index > at) {
			parts.push({ type: 'text', value: text.value.slice(at, found.index) })
		}
		const link: Link = {
			type: 'link',
			url: to,
			children: [{ type: 'text', value: id }],
			data: { hProperties: { className: ['contract'], dataContract: said } },
		}
		parts.push(link)
		at = found.index + id.length
	}
	if (at === 0) {
		return [text]
	}
	if (at < text.value.length) {
		parts.push({ type: 'text', value: text.value.slice(at) })
	}
	return parts
}
