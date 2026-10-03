import type { Blockquote, Paragraph, PhrasingContent, Root } from 'mdast'
import { toString as textOf } from 'mdast-util-to-string'
import { visit } from 'unist-util-visit'

const kinds = ['note', 'tip', 'important', 'warning', 'caution'] as const

/**
 * GitHub's alerts, which GitHub draws itself, become the design's callouts.
 * A bold first line names the callout instead of its kind, and GitHub shows
 * it bold inside its alert:
 *
 *     > [!NOTE]
 *     > **Counters**
 *     > `increase` is for counters.
 *
 * gives a callout labelled "Counters" holding the sentence.
 */
export function calloutsFromAlerts(tree: Root): void {
	visit(tree, 'blockquote', (quote: Blockquote) => {
		const first = quote.children[0]
		if (first?.type !== 'paragraph') {
			return
		}
		const kind = takeMarker(first)
		if (kind === undefined) {
			return
		}
		const label = takeLabel(first) ?? kind

		if (first.children.length === 0) {
			quote.children.shift()
		}
		quote.data = { hName: 'aside', hProperties: { className: ['callout', `callout-${kind}`] } }
		quote.children.unshift({
			type: 'paragraph',
			data: { hProperties: { className: ['callout-label'] } },
			children: [{ type: 'text', value: label }],
		})
	})
}

function takeMarker(paragraph: Paragraph): string | undefined {
	const text = paragraph.children[0]
	if (text?.type !== 'text') {
		return undefined
	}
	const match = /^\[!(\w+)\][ \t]*(?:\r?\n)?/.exec(text.value)
	const kind = match?.[1]?.toLowerCase()
	if (match === null || kind === undefined || !kinds.includes(kind as (typeof kinds)[number])) {
		return undefined
	}
	text.value = text.value.slice(match[0].length)
	dropEmptyStart(paragraph)
	return kind
}

function takeLabel(paragraph: Paragraph): string | undefined {
	const strong = paragraph.children[0]
	if (strong?.type !== 'strong') {
		return undefined
	}
	const next = paragraph.children[1]
	const endsLine =
		next === undefined ||
		next.type === 'break' ||
		(next.type === 'text' && /^\s*\n/.test(next.value))
	if (!endsLine) {
		return undefined
	}
	paragraph.children.shift()
	if (next?.type === 'break') {
		paragraph.children.shift()
	} else if (next?.type === 'text') {
		next.value = next.value.replace(/^\s*\n/, '')
	}
	dropEmptyStart(paragraph)
	return textOf(strong)
}

function dropEmptyStart(paragraph: Paragraph): void {
	const first: PhrasingContent | undefined = paragraph.children[0]
	if (first?.type === 'text' && first.value === '') {
		paragraph.children.shift()
	}
}
