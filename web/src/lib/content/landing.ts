import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { toHtml } from 'hast-util-to-html'
import { h } from 'hastscript'
import type { Code } from 'mdast'

import { byLanguage, copyButton, languageSwitch, panes, toFence } from './code'
import { createHighlight } from './highlight'
import { parse } from './markdown'
import { sourceUrl } from './repo'
import type { Site } from './site'

/**
 * The landing page's sample: a language each, its install command beside the
 * switch and a copy button inside the code. It shares the docs' language
 * switch, so a reader who picks Python here reads Python everywhere.
 */
export async function heroSample(): Promise<string> {
	const tree = parse(readFileSync(join(process.cwd(), 'src/lib/landing/first-look.md'), 'utf8'))
	const fences = tree.children.filter((node): node is Code => node.type === 'code').map(toFence)
	const highlight = await createHighlight()
	const variants = byLanguage(fences)

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

interface Engine {
	name: string
	short: string
	text: string
	/** its guide, when the docs have it */
	guide: string
	/** its contract, which stands in until the guide is written */
	readme: string
}

const engines: Engine[] = [
	{
		name: 'SQL',
		short: 'Relational state',
		text: "The application's own SQL databases: tables from structs, checked migrations.",
		guide: 'docs/store/sql.md',
		readme: 'sqldb/README.md',
	},
	{
		name: 'KV',
		short: 'Application state',
		text: 'Current state: typed buckets, counters, expiry, versions.',
		guide: 'docs/store/kv.md',
		readme: 'kv/README.md',
	},
	{
		name: 'Jobs',
		short: 'Durable background work',
		text: 'Work that runs at its time: retries, leases, repeats.',
		guide: 'docs/store/jobs.md',
		readme: 'jobs/README.md',
	},
	{
		name: 'Blobs',
		short: 'Files and objects',
		text: 'Files by path, checked when read whole.',
		guide: 'docs/store/blobs.md',
		readme: 'blobs/README.md',
	},
	{
		name: 'Records',
		short: 'Logs and events',
		text: 'Read by time, level and keys.',
		guide: 'docs/store/records.md',
		readme: 'records/README.md',
	},
	{
		name: 'Metrics',
		short: 'Time series',
		text: 'Samples kept bit for bit, answered exactly.',
		guide: 'docs/store/metrics.md',
		readme: 'metrics/README.md',
	},
]

export interface EngineRow {
	name: string
	short: string
	text: string
	href: string
	external: boolean
}

export function engineRows(site: Site): EngineRow[] {
	return engines.map(engine => {
		const page = site.pages.find(candidate => candidate.file === engine.guide)
		return {
			name: engine.name,
			short: engine.short,
			text: engine.text,
			href: page?.url ?? sourceUrl(engine.readme, site.commit, 'file'),
			external: page === undefined,
		}
	})
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
