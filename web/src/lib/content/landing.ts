import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { toHtml } from 'hast-util-to-html'
import { h } from 'hastscript'
import type { Code, Paragraph, Root, RootContent, Table, TableCell } from 'mdast'
import { toString as textOf } from 'mdast-util-to-string'

import { byLanguage, copyButton, languageSwitch, panes, toFence } from './code'
import { createHighlight, type Highlight } from './highlight'
import { type Links, resolveLink } from './links'
import { finish, parse, toHast } from './markdown'
import { checkout } from './repo'
import type { Site } from './site'

/** The landing page's words, read when the site is built; its design is +page.svelte. */
export const landingFile = 'web/landing.md'

export interface Landing {
	/** what a browser's tab and a search result show */
	title: string
	headline: string
	/** the paragraph under the headline, as HTML */
	pitch: string
	/** the sample: a language each, its install command beside the switch */
	sample: string
	engines: EngineRow[]
	numbers: Numbers
}

export interface EngineRow {
	name: string
	short: string
	text: string
	href: string
	external: boolean
}

export interface Numbers {
	title: string
	legend: { term: string; text: string }[]
	/** the paragraph under the legend, as HTML, its links resolved */
	caveat: string
}

/**
 * The landing page's words from web/landing.md, so that changing them is
 * editing markdown:
 *
 *     # A small storage runtime for applications.   the headline
 *     SQL, key-value state, …                        the pitch
 *     ```ts install="bun add tinystore"              the sample, a fence a language
 *     ## Engines                                     a row an engine: its link, in short, in a line
 *     ## Measured against what it replaces.          the numbers: the legend's rows, then the caveat
 *
 * A part that is missing, or a link that leads nowhere, fails the build.
 */
export async function readLanding(site: Site, root = checkout()): Promise<Landing> {
	const [intro, ...sections] = sectionsOf(readTree(root))
	const at = sections.findIndex(section => section.title === 'Engines')
	const engines = sections[at]
	const numbers = sections[at + 1]
	if (intro === undefined || engines === undefined || numbers === undefined) {
		throw new Error(
			`${landingFile}: a headline, then "## Engines" and the numbers' section after it`,
		)
	}

	const highlight = await createHighlight()
	const links = linksOf(site, root)
	const headline = headlineOf(intro)

	return {
		title: `TinyStore: ${lowerFirst(withoutStop(headline))}`,
		headline,
		pitch: await inline(paragraphOf(intro), links, highlight),
		sample: sampleOf(
			intro.nodes.filter((node): node is Code => node.type === 'code'),
			highlight,
		),
		engines: rowsOf(tableOf(engines)).map(cells => engineRow(cells, links)),
		numbers: {
			title: numbers.title,
			legend: rowsOf(tableOf(numbers)).map(([term, text]) => ({
				term: plain(term),
				text: plain(text),
			})),
			caveat: await inline(paragraphOf(numbers), links, highlight),
		},
	}
}

/** What TinyStore is, in one sentence: the headline, then the pitch. llms.txt opens with it. */
export function landingSummary(root = checkout()): string {
	const [intro] = sectionsOf(readTree(root))
	if (intro === undefined) {
		throw new Error(`${landingFile} is empty`)
	}
	return `${withoutStop(headlineOf(intro))}: ${plain(paragraphOf(intro))}`
}

interface Section {
	/** the `##` heading's text; the part before the first one has none */
	title: string
	nodes: RootContent[]
}

function readTree(root: string): Root {
	return parse(readFileSync(join(root, landingFile), 'utf8'))
}

function sectionsOf(tree: Root): Section[] {
	const sections: Section[] = [{ title: '', nodes: [] }]
	for (const node of tree.children) {
		if (node.type === 'heading' && node.depth === 2) {
			sections.push({ title: textOf(node), nodes: [] })
		} else {
			sections.at(-1)?.nodes.push(node)
		}
	}
	return sections
}

function headlineOf(intro: Section): string {
	const heading = intro.nodes.find(node => node.type === 'heading' && node.depth === 1)
	if (heading === undefined) {
		throw new Error(`${landingFile} has no headline: its first heading should be a # heading`)
	}
	return textOf(heading)
}

function paragraphOf(section: Section): Paragraph {
	const paragraph = section.nodes.find((node): node is Paragraph => node.type === 'paragraph')
	if (paragraph === undefined) {
		const where = section.title === '' ? 'the headline' : `"## ${section.title}"`
		throw new Error(`${landingFile}: ${where} has no paragraph under it`)
	}
	return paragraph
}

function tableOf(section: Section): Table {
	const table = section.nodes.find((node): node is Table => node.type === 'table')
	if (table === undefined) {
		throw new Error(`${landingFile}: "## ${section.title}" has no table`)
	}
	return table
}

// a table's rows past its header, which GitHub needs and which is left empty here
function rowsOf(table: Table): TableCell[][] {
	return table.children.slice(1).map(row => row.children)
}

function engineRow([name, short, text]: TableCell[], links: Links): EngineRow {
	const link = name?.children.find(node => node.type === 'link')
	if (link === undefined) {
		throw new Error(`${landingFile}: an engine's row begins with a link to its page`)
	}
	const resolved = resolveLink(links, landingFile, link.url, new Set(), 'link')
	if ('problem' in resolved) {
		throw new Error(`${landingFile}: ${resolved.problem}`)
	}
	return {
		name: textOf(link),
		short: plain(short),
		text: plain(text),
		href: resolved.href,
		external: !resolved.href.startsWith('/'),
	}
}

// the docs' pages, as a link from the landing page reaches them
function linksOf(site: Site, root: string): Links {
	return {
		root,
		commit: site.commit,
		pages: new Map(
			site.pages.map(page => [
				page.file,
				{ url: page.url, ids: new Set(page.headings.map(heading => heading.id)) },
			]),
		),
		assets: new Set(),
	}
}

// a paragraph's inside as HTML, its links resolved as a page's are
async function inline(paragraph: Paragraph, links: Links, highlight: Highlight): Promise<string> {
	const tree = await toHast({ type: 'root', children: [paragraph] }, highlight)
	const { html, problems } = finish(tree, links, landingFile, new Set())
	if (problems.length > 0) {
		throw new Error(`${landingFile}: ${problems.map(problem => problem.message).join('; ')}`)
	}
	return html.replace(/^<p>/, '').replace(/<\/p>$/, '')
}

/**
 * The sample: a language each, its install command beside the switch and a
 * copy button inside the code. It shares the docs' language switch, so a
 * reader who picks Python here reads Python everywhere.
 */
function sampleOf(codes: Code[], highlight: Highlight): string {
	const variants = byLanguage(codes.map(toFence))
	if (variants.length === 0) {
		throw new Error(`${landingFile} has no sample: a fence a language under the headline`)
	}

	const group = h(
		'div.code.code-hero',
		{ dataCode: '' },
		variants.map((variant, index) => {
			const install = variant.fences[0]?.install
			return h(
				'div.code-variant',
				{ dataVariant: variant.key, dataDefault: index === 0 ? '' : undefined },
				[
					h('div.code-bar', [
						languageSwitch(variants, variant.key),
						install === undefined
							? h('span')
							: h(
									'button.code-install',
									{ type: 'button', dataCopy: '', dataCopyText: install, title: 'Copy' },
									[
										h('span.command', [h('span.prompt', '$'), ` ${install}`]),
										h('span.done', '✓ Copied'),
									],
								),
					]),
					h('div.code-body', [copyButton(), ...panes(variant.fences, highlight)]),
				],
			)
		}),
	)

	return toHtml(group)
}

function plain(node: TableCell | Paragraph | undefined): string {
	return node === undefined ? '' : textOf(node).replace(/\s+/g, ' ').trim()
}

function withoutStop(text: string): string {
	return text.replace(/\.$/, '')
}

function lowerFirst(text: string): string {
	return text.charAt(0).toLowerCase() + text.slice(1)
}

/** A benchmark card as the README draws it, read back from its SVG. */
export interface Card {
	title: string
	unit: string
	rows: Row[]
}

export interface Row {
	label: string
	/** TinyStore's own row, which the design draws in the accent */
	ours: boolean
	/** one bar, or two: idle solid, then the load peak faded */
	bars: { value: string; fraction: number; faded: boolean }[]
}

/**
 * The README's benchmark cards, in its order, read from the SVGs it shows, so
 * that the landing page and the README cannot disagree on a number. A card
 * whose SVG no longer reads as a card fails the build.
 */
export function readCards(root: string): Card[] {
	const readme = readFileSync(join(root, 'README.md'), 'utf8')
	const numbers = readme.slice(
		readme.indexOf('## Numbers'),
		readme.indexOf('<details>', readme.indexOf('## Numbers')),
	)
	const files = [...numbers.matchAll(/srcset="(\.github\/assets\/bench-[\w-]+-dark\.svg)"/g)].map(
		match => match[1] ?? '',
	)

	return files.map(file => {
		try {
			return readCard(readFileSync(join(root, file), 'utf8'))
		} catch (error) {
			throw new Error(`${file}: ${error instanceof Error ? error.message : String(error)}`)
		}
	})
}

/**
 * One card: a title at font-size 16, its unit at 12, then each row's label at
 * x=20 followed by its bars, each a rect and the value after it.
 *
 *     <text x="20" y="89" font-weight="600">TinyStore</text>   row, ours
 *     <rect width="153" fill="#ECEAF3"/> <text>19 k</text>      bar, 153 of the widest
 */
export function readCard(svg: string): Card {
	const elements = [...svg.matchAll(/<(text|rect)\b([^>]*?)(?:\/>|>([^<]*)<\/text>)/g)]
	let title = ''
	let unit = ''
	const rows: Row[] = []
	let width: number | undefined
	let faded = false
	let highlighted = false

	for (const [, tag, attributes = '', content = ''] of elements) {
		const attribute = (name: string) => new RegExp(`\\b${name}="([^"]*)"`).exec(attributes)?.[1]
		if (tag === 'rect') {
			if (attribute('fill') === 'none') {
				continue
			}
			width = Number(attribute('width'))
			faded = Number(attribute('opacity') ?? '1') < 1
			// the neutral bars are GitHub's border grey; TinyStore's are drawn in the text colour
			highlighted = attribute('fill')?.toLowerCase() !== '#3d444d'
			continue
		}
		const text = decode(content)
		if (attribute('font-size') === '16') {
			title = text
		} else if (attribute('font-size') === '12' && attribute('y') === '56') {
			unit = text
		} else if (attribute('x') === '20') {
			rows.push({ label: text, ours: attribute('font-weight') === '600', bars: [] })
		} else if (width !== undefined) {
			const row = rows.at(-1)
			row?.bars.push({ value: text, fraction: width, faded })
			if (row !== undefined && highlighted) {
				row.ours = true
			}
			width = undefined
		}
	}

	const widest = Math.max(...rows.flatMap(row => row.bars.map(bar => bar.fraction)))
	if (title === '' || rows.length === 0 || !(widest > 0)) {
		throw new Error('reads as no benchmark card: a title, rows and their bars')
	}
	for (const row of rows) {
		for (const bar of row.bars) {
			bar.fraction /= widest
		}
	}
	return { title, unit, rows }
}

function decode(text: string): string {
	return text
		.replace(/&lt;/g, '<')
		.replace(/&gt;/g, '>')
		.replace(/&quot;/g, '"')
		.replace(/&#(\d+);/g, (_, code: string) => String.fromCodePoint(Number(code)))
		.replace(/&amp;/g, '&')
		.trim()
}
