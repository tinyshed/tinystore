import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import type { Code, Image, Link, RootContent, Table } from 'mdast'
import { toString as textOf } from 'mdast-util-to-string'
import { visit } from 'unist-util-visit'

import { repository } from '../site'
import { byLanguage, toFence } from './code'
import { landingFile, linksOf, sectionsOf } from './landing'
import { resolveLink } from './links'
import { parse } from './markdown'
import { outline } from './outline'
import { checkout, resolveFrom } from './repo'
import type { Site } from './site'

export const readmeFile = 'README.md'

/**
 * A README that shows the landing page's words: the repository's, and each
 * SDK package's, which npm and PyPI show without the rest of the repository.
 * A package shows its own language alone and links to GitHub, because
 * neither registry follows a link relative to the README.
 */
export interface Readme {
	file: string
	languages: string[]
	page: 'repository' | 'package'
}

// GitHub has no language switch, so the language that embeds TinyStore shows and the others fold
export const readmes: Readme[] = [
	{ file: readmeFile, languages: ['go', 'bun', 'python'], page: 'repository' },
	{ file: 'sdk/js/README.md', languages: ['bun'], page: 'package' },
	{ file: 'sdk/python/README.md', languages: ['python'], page: 'package' },
]

const repositoryReadme = readmes[0] as Readme
const onGitHub = `${repository}/blob/main/`
const absolute = /^[a-z][a-z\d+.-]*:/i

/**
 * A README with the landing page's words written into it from
 * web/landing.md, so that the repository's front page, the packages' pages
 * and the site's say the same thing. Each part lies between two markers, and
 * whatever is between them is replaced:
 *
 *     <!-- landing:headline -->   the headline and the pitch, centred under the logo
 *     <!-- landing:sample -->     the first language open, the others folded, then each install command
 *     <!-- landing:engines -->    the engines' table, its links rebased from web/ to the top, or to GitHub
 *
 * and each ends at its `<!-- /landing:… -->`.
 */
export function readmeFromLanding(
	readme: string,
	landing: string,
	shape = repositoryReadme,
): string {
	const [intro, ...sections] = sectionsOf(parse(landing))
	const engines = sections.find(section => section.title === 'Engines')
	if (intro === undefined || engines === undefined) {
		throw new Error(`${landingFile}: a sample under the headline, then "## Engines"`)
	}

	const parts = {
		headline: shape.page === 'repository' ? headlineOf(intro.nodes) : plainHeadlineOf(intro.nodes),
		sample: sampleOf(intro.nodes, shape.languages),
		engines: tableOf(engines.nodes, landing, shape.page),
	}
	return Object.entries(parts).reduce(
		(text, [name, part]) => replacePart(text, shape.file, name, part),
		readme,
	)
}

/** Writes every README's parts from web/landing.md, as `task readme` does, and names those that changed. */
export function writeReadmes(root = checkout()): string[] {
	const landing = readFileSync(join(root, landingFile), 'utf8')
	return readmes.flatMap(shape => {
		const path = join(root, shape.file)
		const readme = readFileSync(path, 'utf8')
		const written = readmeFromLanding(readme, landing, shape)
		if (written === readme) {
			return []
		}
		writeFileSync(path, written)
		return [shape.file]
	})
}

/**
 * What is wrong with each link of a README's markdown that leads nowhere. A
 * package's link to GitHub's main is checked as the file it names, and a
 * relative one is refused, because npm and PyPI show it leading nowhere.
 */
export function readmeProblems(
	readme: string,
	site: Site,
	root = checkout(),
	shape = repositoryReadme,
): string[] {
	const tree = parse(readme)
	const { ids } = outline(tree)
	const links = linksOf(site, root)

	const problems: string[] = []
	visit(tree, ['link', 'image'], node => {
		const { type, url } = node as Link | Image
		if (shape.page === 'package' && !absolute.test(url) && !url.startsWith('#')) {
			problems.push(`${url} is relative, which npm and PyPI cannot follow`)
			return
		}
		const fromTop = url.startsWith(onGitHub) ? url.slice(onGitHub.length) : url
		const resolved = resolveLink(
			links,
			readmeFile,
			fromTop,
			ids,
			type === 'image' ? 'asset' : 'link',
		)
		if ('problem' in resolved) {
			problems.push(resolved.problem)
		}
	})
	return problems
}

function replacePart(readme: string, file: string, name: string, part: string): string {
	const open = `<!-- landing:${name} -->`
	const close = `<!-- /landing:${name} -->`
	const start = readme.indexOf(open)
	const end = readme.indexOf(close, start)
	if (start === -1 || end === -1) {
		throw new Error(`${file} has no ${open} … ${close} for the landing page's ${name}`)
	}
	return `${readme.slice(0, start + open.length)}\n\n${part}\n\n${readme.slice(end)}`
}

function headlineAndPitch(nodes: RootContent[]): [RootContent, RootContent] {
	const headline = nodes.find(node => node.type === 'heading' && node.depth === 1)
	const pitch = nodes.find(node => node.type === 'paragraph')
	if (headline === undefined || pitch === undefined) {
		throw new Error(`${landingFile}: a # headline, then the pitch's paragraph`)
	}
	return [headline, pitch]
}

// plain text, since GitHub reads no markdown inside the HTML that centres it
function headlineOf(nodes: RootContent[]): string {
	const [headline, pitch] = headlineAndPitch(nodes)
	return [`<b>${htmlOf(headline)}</b>`, htmlOf(pitch)]
		.map(line => `<p align="center">\n  ${line}\n</p>`)
		.join('\n\n')
}

// npm drops the HTML that centres, so a package's page says it in markdown, under the package's own title
function plainHeadlineOf(nodes: RootContent[]): string {
	const [headline, pitch] = headlineAndPitch(nodes)
	return `**${oneLine(headline)}** ${oneLine(pitch)}`
}

function oneLine(node: RootContent): string {
	return textOf(node).replace(/\s+/g, ' ').trim()
}

function htmlOf(node: RootContent): string {
	return oneLine(node).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
}

function sampleOf(nodes: RootContent[], shown: string[]): string {
	const codes = nodes.filter((node): node is Code => node.type === 'code')
	const variants = byLanguage(codes.map(toFence))
		.filter(variant => shown.includes(variant.key))
		.sort((a, b) => shown.indexOf(a.key) - shown.indexOf(b.key))
	if (variants.length === 0) {
		throw new Error(
			`${landingFile} has no sample in ${shown.join(', ')}: a fence a language under the headline`,
		)
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

function fenced(lang: string, code: string): string {
	return `\`\`\`${lang}\n${code}\n\`\`\``
}

function folded(label: string, body: string): string {
	return `<details>\n<summary><b>${label}</b></summary>\n\n${body}\n\n</details>`
}

// the engines' table as it is written, each link rebased from web/ to the top of the repository, or to GitHub
function tableOf(nodes: RootContent[], landing: string, page: Readme['page']): string {
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
		(text, url) =>
			text.replaceAll(`](${url})`, `](${page === 'package' ? toGitHub(url) : fromTop(url)})`),
		landing.slice(start, end),
	)
}

// a link written in web/landing.md, as it reads from the top of the repository
function fromTop(url: string): string {
	if (absolute.test(url) || url.startsWith('#') || url.startsWith('/')) {
		return url
	}
	return resolveFrom(landingFile, url) ?? url
}

// a link written in web/landing.md, as a page outside the repository reaches it
function toGitHub(url: string): string {
	if (absolute.test(url) || url.startsWith('#')) {
		return url
	}
	return `${onGitHub}${fromTop(url)}`
}

if (import.meta.main) {
	for (const file of writeReadmes()) {
		process.stdout.write(`${file}: the landing page's words written from ${landingFile}\n`)
	}
}
