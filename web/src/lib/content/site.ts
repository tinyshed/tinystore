import { readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import type { Root } from 'mdast'

import { calloutsFromAlerts } from './alerts'
import { groupFences } from './code'
import { createHighlight, type Highlight } from './highlight'
import { landingFile, landingSummary } from './landing'
import type { Links, Target } from './links'
import { exportMarkdown, finish, type Problem, parse, toHast } from './markdown'
import { type Index, indexFile, type NavSection, readIndex, urlOf } from './nav'
import { outline, type Section, sections } from './outline'
import { checkout, editUrl, headCommit, lastChanged, sourceUrl } from './repo'

/** What kind of page a section of the index holds, which the search filters by. */
export type Kind = 'guide' | 'reference' | 'design'

export interface Neighbour {
	title: string
	url: string
}

export interface Page {
	slug: string
	url: string
	/** its file, as a path of the repository */
	file: string
	title: string
	lead?: string
	html: string
	headings: { id: string; text: string }[]
	/** the index's section the page is listed under, its breadcrumb */
	section: string
	kind: Kind
	/** when the file last changed, from the history */
	updated?: string
	editUrl: string
	sourceUrl: string
	previous?: Neighbour
	next?: Neighbour
	/** built, and listed nowhere: not in the sidebar, the search, the sitemap or llms.txt */
	hidden?: true
	/** the page as markdown that stands on its own: links absolute, the site's comments gone */
	markdown: string
	sections: Section[]
}

export interface Site {
	title: string
	/** what TinyStore is, in a sentence: the landing page's headline and pitch */
	summary: string
	nav: NavSection[]
	/** the index first, at /docs, then every page in the index's order */
	pages: Page[]
	/** files the pages show, which the site serves under /files/ */
	assets: string[]
	commit: string
	origin: string
}

/** Where the site will be read; canonical links, the sitemap and llms.txt are made absolute with it. */
export function siteOrigin(): string {
	return (process.env.SITE_ORIGIN ?? 'http://localhost:3000').replace(/\/$/, '')
}

/** Every element a page can hold, built at /docs/kitchen-sink and listed nowhere. */
const kitchenSink = 'web/kitchen-sink.md'

interface Entry {
	file: string
	slug: string
	section: string
	kind: Kind
	hidden?: boolean
}

let cached: { stamp: string; site: Promise<Site> } | undefined

/**
 * The whole site, read once a build. The dev server asks on every request,
 * so the files' times decide whether the cached one is still the docs as
 * they are.
 */
export function loadSite(root = checkout()): Promise<Site> {
	const stamp = stampOf(root)
	if (cached?.stamp !== stamp) {
		cached = { stamp, site: buildSite(root) }
	}
	return cached.site
}

async function buildSite(root: string): Promise<Site> {
	const commit = headCommit(root)
	const origin = siteOrigin()
	const highlight = await createHighlight()

	const indexTree = parse(read(root, indexFile))
	const index = readIndex(indexTree, root, commit)

	const drafts = await Promise.all(
		entriesOf(index).map(entry =>
			draft(entry, entry.file === indexFile ? indexTree : parse(read(root, entry.file)), highlight),
		),
	)

	const links: Links = {
		root,
		commit,
		pages: new Map<string, Target>(
			drafts.map(page => [page.file, { url: page.url, ids: page.ids }]),
		),
		assets: new Set(),
	}

	const finished = drafts.map(page => finishPage(page, links, { root, commit, origin }))
	const problems = finished.flatMap(result => result.problems)
	if (problems.length > 0) {
		throw new Error(
			`the docs link to what does not exist:\n${problems.map(p => `  ${p}`).join('\n')}`,
		)
	}

	const pages = finished.map(result => result.page)
	linkNeighbours(pages)

	return {
		title: index.title,
		summary: landingSummary(root),
		nav: index.sections,
		pages,
		assets: [...links.assets].sort(),
		commit,
		origin,
	}
}

/** The index itself, then every page it links, in its order, then the kitchen sink. */
function entriesOf(index: Index): Entry[] {
	const linked = index.sections.flatMap(section =>
		section.items.flatMap(item =>
			item.kind === 'page'
				? [
						{
							file: item.file,
							slug: item.slug,
							section: section.title,
							kind: kindOf(section.title),
						},
					]
				: [],
		),
	)
	return [
		{ file: indexFile, slug: '', section: '', kind: 'guide' },
		...linked,
		{ file: kitchenSink, slug: 'kitchen-sink', section: '', kind: 'guide', hidden: true },
	]
}

type Draft = Awaited<ReturnType<typeof draft>>

async function draft(entry: Entry, tree: Root, highlight: Highlight) {
	// the exported markdown is the file as written: its title, its alerts, its fences
	const exported = structuredClone(tree)

	calloutsFromAlerts(tree)
	const { title, lead, headings, ids } = outline(tree)
	const found = sections(tree)
	groupFences(tree)

	if (title === '') {
		throw new Error(`${entry.file} has no title: its first line should be a # heading`)
	}

	return {
		...entry,
		url: urlOf(entry.slug),
		title,
		...(lead === undefined ? {} : { lead }),
		headings,
		ids,
		sections: found,
		tree: exported,
		hast: await toHast(tree, highlight),
	}
}

/** A drafted page made final once every page is known, so that its links can be checked. */
function finishPage(
	page: Draft,
	links: Links,
	built: { root: string; commit: string; origin: string },
): { page: Page; problems: string[] } {
	const finished = finish(page.hast, links, page.file, page.ids)
	const updated = lastChanged(built.root, page.file)

	return {
		problems: finished.problems.map(problem => describe(page.file, problem)),
		page: {
			slug: page.slug,
			url: page.url,
			file: page.file,
			title: page.title,
			...(page.lead === undefined ? {} : { lead: page.lead }),
			html: finished.html,
			headings: page.headings,
			section: page.section,
			kind: page.kind,
			...(updated === undefined ? {} : { updated }),
			editUrl: editUrl(page.file),
			sourceUrl: sourceUrl(page.file, built.commit, 'file'),
			markdown: exportMarkdown(page.tree, links, page.file, page.ids, built.origin),
			sections: page.sections,
			...(page.hidden === true ? { hidden: true as const } : {}),
		},
	}
}

// the index's first page is the index itself, which has no neighbours of its own
function linkNeighbours(pages: Page[]): void {
	const listed = pages.filter(page => page.slug !== '' && page.hidden !== true)
	listed.forEach((page, at) => {
		const before = listed[at - 1]
		const after = listed[at + 1]
		if (before !== undefined) {
			page.previous = { title: before.title, url: before.url }
		}
		if (after !== undefined) {
			page.next = { title: after.title, url: after.url }
		}
	})
}

function kindOf(section: string): Kind {
	const name = section.toLowerCase()
	if (name === 'reference') {
		return 'reference'
	}
	return name === 'design' ? 'design' : 'guide'
}

function describe(file: string, problem: Problem): string {
	return `${file}${problem.line === undefined ? '' : `:${problem.line}`}: ${problem.message}`
}

function read(root: string, file: string): string {
	return readFileSync(join(root, file), 'utf8')
}

function stampOf(root: string): string {
	try {
		const index = read(root, indexFile)
		const files = [
			indexFile,
			kitchenSink,
			landingFile,
			...[...index.matchAll(/\]\(([^)#\s]+\.md)/g)].map(m => `docs/${m[1]}`),
		]
		return files
			.map(file => {
				try {
					return statSync(join(root, file)).mtimeMs
				} catch {
					return 0
				}
			})
			.join(':')
	} catch {
		return 'missing'
	}
}
