import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import type { Code, Image, Link, RootContent, Table } from 'mdast'
import { toString as textOf } from 'mdast-util-to-string'
import { visit } from 'unist-util-visit'

import { byLanguage, toFence } from './code'
import { landingFile, linksOf, sectionsOf } from './landing'
import { resolveLink } from './links'
import { parse } from './markdown'
import { outline } from './outline'
import { checkout, resolveFrom } from './repo'
import type { Site } from './site'

export const readmeFile = 'README.md'

// GitHub has no language switch, so the language that embeds TinyStore shows and the others fold
const shownFirst = ['go', 'bun', 'python']

/**
 * The README with the landing page's words written into it from
 * web/landing.md, so that the repository's front page and the site's say the
 * same thing. Each part lies between two markers, and whatever is between
 * them is replaced:
 *
 *     <!-- landing:headline -->   the headline and the pitch, centred under the logo
 *     <!-- landing:sample -->     Go open, the other languages folded, then each install command
 *     <!-- landing:engines -->    the engines' table, its links rebased from web/ to the top
 *
 * and each ends at its `<!-- /landing:… -->`.
 */
export function readmeFromLanding(readme: string, landing: string): string {
	const [intro, ...sections] = sectionsOf(parse(landing))
	const engines = sections.find(section => section.title === 'Engines')
	if (intro === undefined || engines === undefined) {
		throw new Error(`${landingFile}: a sample under the headline, then "## Engines"`)
	}

	const parts = {
		headline: headlineOf(intro.nodes),
		sample: sampleOf(intro.nodes),
		engines: tableOf(engines.nodes, landing),
	}
	return Object.entries(parts).reduce((text, [name, part]) => replacePart(text, name, part), readme)
}

/** Writes the README's parts from web/landing.md, as `task readme` does, and says whether they changed. */
export function writeReadme(root = checkout()): boolean {
	const path = join(root, readmeFile)
	const readme = readFileSync(path, 'utf8')
	const written = readmeFromLanding(readme, readFileSync(join(root, landingFile), 'utf8'))
	if (written !== readme) {
		writeFileSync(path, written)
	}
	return written !== readme
}

/** What is wrong with each link of the README's markdown that leads nowhere. */
export function readmeProblems(readme: string, site: Site, root = checkout()): string[] {
	const tree = parse(readme)
	const { ids } = outline(tree)
	const links = linksOf(site, root)

	const problems: string[] = []
	visit(tree, ['link', 'image'], node => {
		const { type, url } = node as Link | Image
		const resolved = resolveLink(links, readmeFile, url, ids, type === 'image' ? 'asset' : 'link')
		if ('problem' in resolved) {
			problems.push(resolved.problem)
		}
	})
	return problems
}

function replacePart(readme: string, name: string, part: string): string {
	const open = `<!-- landing:${name} -->`
	const close = `<!-- /landing:${name} -->`
	const start = readme.indexOf(open)
	const end = readme.indexOf(close, start)
	if (start === -1 || end === -1) {
		throw new Error(`${readmeFile} has no ${open} … ${close} for the landing page's ${name}`)
	}
	return `${readme.slice(0, start + open.length)}\n\n${part}\n\n${readme.slice(end)}`
}

// plain text, since GitHub reads no markdown inside the HTML that centres it
function headlineOf(nodes: RootContent[]): string {
	const headline = nodes.find(node => node.type === 'heading' && node.depth === 1)
	const pitch = nodes.find(node => node.type === 'paragraph')
	if (headline === undefined || pitch === undefined) {
		throw new Error(`${landingFile}: a # headline, then the pitch's paragraph`)
	}
	return [`<b>${htmlOf(headline)}</b>`, htmlOf(pitch)]
		.map(line => `<p align="center">\n  ${line}\n</p>`)
		.join('\n\n')
}

function htmlOf(node: RootContent): string {
	const text = textOf(node).replace(/\s+/g, ' ').trim()
	return text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
}

function sampleOf(nodes: RootContent[]): string {
	const codes = nodes.filter((node): node is Code => node.type === 'code')
	const variants = byLanguage(codes.map(toFence)).sort((a, b) => rank(a.key) - rank(b.key))
	if (variants.length === 0) {
		throw new Error(`${landingFile} has no sample: a fence a language under the headline`)
	}

	const blocks = variants.map((variant, index) => {
		const code = variant.fences.map(fence => fenced(fence.lang, fence.code)).join('\n\n')
		return index === 0 ? code : folded(variant.label, code)
	})
	const installs = variants.flatMap(variant => variant.fences[0]?.install ?? [])
	if (installs.length > 0) {
		blocks.push(fenced('sh', installs.join('\n')))
	}
	return blocks.join('\n\n')
}

function rank(language: string): number {
	const at = shownFirst.indexOf(language)
	return at === -1 ? shownFirst.length : at
}

function fenced(lang: string, code: string): string {
	return `\`\`\`${lang}\n${code}\n\`\`\``
}

function folded(label: string, body: string): string {
	return `<details>\n<summary><b>${label}</b></summary>\n\n${body}\n\n</details>`
}

// the engines' table as it is written, each link rebased from web/ to the top of the repository
function tableOf(nodes: RootContent[], landing: string): string {
	const table = nodes.find((node): node is Table => node.type === 'table')
	const start = table?.position?.start.offset
	const end = table?.position?.end.offset
	if (table === undefined || start === undefined || end === undefined) {
		throw new Error(`${landingFile}: "## Engines" has no table`)
	}

	const urls = new Set<string>()
	visit(table, 'link', link => {
		urls.add(link.url)
	})
	return [...urls].reduce(
		(text, url) => text.replaceAll(`](${url})`, `](${fromTop(url)})`),
		landing.slice(start, end),
	)
}

// a link written in web/landing.md, as it reads from the top of the repository
function fromTop(url: string): string {
	if (/^[a-z][a-z\d+.-]*:/i.test(url) || url.startsWith('#') || url.startsWith('/')) {
		return url
	}
	return resolveFrom(landingFile, url) ?? url
}

if (import.meta.main && writeReadme()) {
	process.stdout.write(`${readmeFile}: the landing page's words written from ${landingFile}\n`)
}
