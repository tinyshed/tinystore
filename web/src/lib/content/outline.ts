import GithubSlugger from 'github-slugger'
import type { Heading, Nodes, Paragraph, Root } from 'mdast'
import { toString as textOf } from 'mdast-util-to-string'
import { visit } from 'unist-util-visit'

export interface Outline {
	/** the first top-level heading, which the page shows above its lead */
	title: string
	/** the paragraph right under the title, as plain text, for the meta description */
	lead?: string
	/** the second-level headings, as "On this page" lists them */
	headings: { id: string; text: string }[]
	/** every id a link may point at */
	ids: Set<string>
}

/**
 * Gives every heading the id GitHub gives it, so that a link written for
 * GitHub, `server.md#sdks`, lands on the same heading here; then takes the
 * title out of the tree, since the page draws it, and marks the lead.
 */
export function outline(tree: Root): Outline {
	const slugger = new GithubSlugger()
	const ids = new Set<string>()
	const headings: Outline['headings'] = []

	visit(tree, 'heading', (heading: Heading) => {
		const text = textOf(heading)
		const id = slugger.slug(text)
		heading.data = { ...heading.data, hProperties: { ...heading.data?.hProperties, id } }
		ids.add(id)
		if (heading.depth === 2) {
			headings.push({ id, text })
		}
	})

	const at = tree.children.findIndex(node => node.type === 'heading' && node.depth === 1)
	const title = at === -1 ? '' : textOf(tree.children[at] as Heading)
	if (at !== -1) {
		tree.children.splice(at, 1)
	}

	const lead = tree.children[at === -1 ? 0 : at]
	if (lead?.type === 'paragraph' && at !== -1) {
		markLead(lead)
		return { title, lead: collapse(textOf(lead)), headings, ids }
	}
	return { title, headings, ids }
}

/** A part of the page between two headings, as the search finds it. */
export interface Section {
	heading: string
	id?: string
	text: string
	code: string
}

/** The page cut at its second- and third-level headings, prose apart from code. */
export function sections(tree: Root): Section[] {
	const found: Section[] = [{ heading: '', text: '', code: '' }]

	for (const node of tree.children) {
		if (node.type === 'heading' && (node.depth === 2 || node.depth === 3)) {
			const id = node.data?.hProperties?.id
			found.push({
				heading: textOf(node),
				...(typeof id === 'string' ? { id } : {}),
				text: '',
				code: '',
			})
			continue
		}
		const current = found.at(-1)
		if (current !== undefined) {
			collect(node, current)
		}
	}

	return found
		.map(section => ({ ...section, text: collapse(section.text), code: section.code.trim() }))
		.filter(section => section.heading !== '' || section.text !== '' || section.code !== '')
}

function collect(node: Nodes, into: Section): void {
	if (node.type === 'code') {
		into.code += `${node.value}\n`
		return
	}
	if (node.type === 'html') {
		return
	}
	if ('value' in node) {
		into.text += `${node.value} `
		return
	}
	if ('children' in node) {
		for (const child of node.children) {
			collect(child as Nodes, into)
		}
		into.text += ' '
	}
}

function markLead(paragraph: Paragraph): void {
	paragraph.data = { ...paragraph.data, hProperties: { className: ['lead'] } }
}

function collapse(text: string): string {
	return text.replace(/\s+/g, ' ').trim()
}
