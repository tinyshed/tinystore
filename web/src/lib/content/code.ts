import type { Element, ElementContent } from 'hast'
import { h, s } from 'hastscript'
import type { Code, Data, Parent, Root, RootContent } from 'mdast'

import type { Highlight } from './highlight'

/** The languages a reader picks once for the whole site, in the design's order. */
export const languages = ['bun', 'python', 'go'] as const

export type Language = (typeof languages)[number]

export const languageLabels: Record<Language, string> = { bun: 'Bun', python: 'Python', go: 'Go' }

// a fence's language, as the language switch knows it
const fenceLanguages: Record<string, Language> = {
	ts: 'bun',
	typescript: 'bun',
	js: 'bun',
	javascript: 'bun',
	python: 'python',
	py: 'python',
	go: 'go',
}

/** One fence: its code, its language and what its info string says after the language. */
export interface Fence {
	lang: string
	code: string
	/** `title="metrics.ts"`: the file's name, shown as its tab */
	title?: string
	/** `for=bun`: which language a fence belongs to when its own does not say, a shell command's */
	language?: Language
	/** `install="bun add tinystore"`: the command the landing page shows beside its sample */
	install?: string
}

/** Fences that read as one block: a language each, and a file each within a language. */
export interface CodeGroup {
	type: 'codeGroup'
	fences: Fence[]
	data?: Data
}

declare module 'mdast' {
	interface RootContentMap {
		codeGroup: CodeGroup
	}
}

/**
 * Replaces every run of adjacent fences with the groups it reads as. On GitHub
 * the fences stay one under another; here they become one block:
 *
 *     ```go             ```ts title="a.ts"
 *     ```ts             ```ts title="b.ts"
 *     ```python         ```python title="a.py"
 *     one block,        one block, Bun and Python,
 *     three languages   two files each
 *
 * Two untitled fences of one language stay two blocks, and a shell command
 * joins a language only when it says which, `for=bun`.
 */
export function groupFences(tree: Root): void {
	visitParents(tree, parent => {
		const children: RootContent[] = []
		let run: Fence[] = []

		const flush = () => {
			for (const fences of splitRun(run)) {
				children.push({ type: 'codeGroup', fences })
			}
			run = []
		}

		for (const child of parent.children as RootContent[]) {
			if (child.type === 'code') {
				run.push(toFence(child))
				continue
			}
			flush()
			children.push(child)
		}
		flush()

		parent.children = children as Parent['children']
	})
}

export function splitRun(run: Fence[]): Fence[][] {
	const groups: Fence[][] = []
	for (const fence of run) {
		const current = groups.at(-1)
		if (current !== undefined && joins(current, fence)) {
			current.push(fence)
		} else {
			groups.push([fence])
		}
	}
	return groups
}

function joins(group: Fence[], fence: Fence): boolean {
	const first = group[0]
	if (first === undefined) {
		return false
	}
	if (first.language === undefined || fence.language === undefined) {
		// plain fences join only as titled files of one language
		return (
			first.language === fence.language &&
			first.lang === fence.lang &&
			fence.title !== undefined &&
			group.every(member => member.title !== undefined)
		)
	}
	const sameLanguage = group.filter(member => member.language === fence.language)
	return (
		sameLanguage.length === 0 ||
		(fence.title !== undefined && sameLanguage.every(member => member.title !== undefined))
	)
}

export function toFence(node: Code): Fence {
	const lang = (node.lang ?? 'text').toLowerCase()
	const attributes = parseMeta(node.meta ?? '')
	const named = attributes.for as Language | undefined
	const language = named !== undefined && languages.includes(named) ? named : fenceLanguages[lang]

	return {
		lang,
		code: node.value,
		...(attributes.title === undefined ? {} : { title: attributes.title }),
		...(language === undefined ? {} : { language }),
		...(attributes.install === undefined ? {} : { install: attributes.install }),
	}
}

/** `title="metrics.ts" for=bun` → `{ title: 'metrics.ts', for: 'bun' }` */
export function parseMeta(meta: string): Record<string, string> {
	const attributes: Record<string, string> = {}
	for (const match of meta.matchAll(/([\w-]+)=(?:"([^"]*)"|'([^']*)'|(\S+))/g)) {
		const [, name, double, single, bare] = match
		if (name !== undefined) {
			attributes[name] = double ?? single ?? bare ?? ''
		}
	}
	return attributes
}

/** A group's fences by language, in the switch's order, a plain group as its one language. */
export function byLanguage(fences: Fence[]): { key: string; label: string; fences: Fence[] }[] {
	const keys = [...new Set(fences.map(fence => fence.language ?? fence.lang))]
	keys.sort((a, b) => order(a) - order(b))

	return keys.map(key => ({
		key,
		label: languageLabels[key as Language] ?? key,
		fences: fences.filter(fence => (fence.language ?? fence.lang) === key),
	}))
}

function order(key: string): number {
	const index = languages.indexOf(key as Language)
	return index === -1 ? languages.length : index
}

/**
 * The block as the page shows it. Every language is in the HTML, the first
 * marked data-default; the stylesheet shows the one the reader picked, which
 * the root element names before the first paint, so nothing flashes and the
 * page needs no script until the reader clicks.
 */
export function renderGroup(group: CodeGroup, highlight: Highlight): Element {
	const variants = byLanguage(group.fences)
	const switcher = variants.length > 1 ? variants : []

	return h(
		'div.code',
		{ dataCode: '' },
		variants.map((variant, index) =>
			h(
				'div.code-variant',
				{ dataVariant: variant.key, dataDefault: index === 0 ? '' : undefined },
				[
					h('div.code-bar', [
						files(variant.fences, switcher.length === 0),
						h('div.code-tools', [languageSwitch(switcher, variant.key), copyButton()]),
					]),
					...panes(variant.fences, highlight),
				],
			),
		),
	)
}

export function panes(fences: Fence[], highlight: Highlight): Element[] {
	return fences.map((fence, index) => {
		const pre = highlight(fence.code, fence.lang)
		pre.properties.dataFile = String(index)
		if (index > 0) {
			pre.properties.hidden = true
		}
		return pre
	})
}

// a block with no switch and no file name says its language, unless it is plain text
function files(fences: Fence[], alone: boolean): ElementContent {
	if (fences.length > 1) {
		return h(
			'div.code-files',
			{ role: 'tablist' },
			fences.map((fence, index) =>
				h(
					'button.code-file',
					{
						type: 'button',
						role: 'tab',
						dataFile: String(index),
						ariaSelected: String(index === 0),
					},
					fence.title ?? fence.lang,
				),
			),
		)
	}
	const first = fences[0]
	const named = first?.title ?? (alone && first?.lang !== 'text' ? first?.lang : undefined)
	return h('span.code-title', named ?? '')
}

export function languageSwitch(
	variants: { key: string; label: string }[],
	current: string,
): ElementContent {
	if (variants.length === 0) {
		return h('span.code-langs')
	}
	return h(
		'div.code-langs',
		variants.map(variant =>
			h(
				'button.code-lang',
				{ type: 'button', dataLang: variant.key, ariaPressed: String(variant.key === current) },
				variant.label,
			),
		),
	)
}

export function copyButton(): Element {
	return h(
		'button.code-copy',
		{ type: 'button', dataCopy: '', title: 'Copy', ariaLabel: 'Copy code' },
		[
			s(
				'svg.icon-copy',
				{
					width: 15,
					height: 15,
					viewBox: '0 0 16 16',
					fill: 'none',
					stroke: 'currentColor',
					strokeWidth: 1.5,
					strokeLinecap: 'round',
					strokeLinejoin: 'round',
					ariaHidden: 'true',
				},
				[
					s('rect', { x: 5.5, y: 5.5, width: 9, height: 9, rx: 2 }),
					s('path', {
						d: 'M10.5 3.5v-.5a1.5 1.5 0 0 0-1.5-1.5H3A1.5 1.5 0 0 0 1.5 3v6A1.5 1.5 0 0 0 3 10.5h.5',
					}),
				],
			),
			s(
				'svg.icon-done',
				{
					width: 15,
					height: 15,
					viewBox: '0 0 16 16',
					fill: 'none',
					stroke: 'currentColor',
					strokeWidth: 1.8,
					strokeLinecap: 'round',
					strokeLinejoin: 'round',
					ariaHidden: 'true',
				},
				[s('path', { d: 'M3 8.5l3.5 3.5L13 4.5' })],
			),
		],
	)
}

function visitParents(node: Root | Parent, visit: (parent: Parent) => void): void {
	visit(node)
	for (const child of node.children) {
		if ('children' in child) {
			visitParents(child as Parent, visit)
		}
	}
}
