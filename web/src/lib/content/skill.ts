import { createHash } from 'node:crypto'

import { type ApiEngine, apiEngines, apiFile, apiLanguageNames, apiLanguages } from './api'
import { llmsFiles } from './llms'
import { parse, stringify } from './markdown'
import type { Page, Site } from './site'

/** The skill's name, and its directory under /.well-known/agent-skills/. */
export const skillName = 'tinystore'

// the docs' first program, whose first blocks install TinyStore in each language
const installPage = 'docs/handbook/start/first-store.md'

/**
 * skill.md, a skill as agentskills.io defines one: what TinyStore is, then
 * how each language installs it and each engine's daily calls. Every block
 * is the first of a page of the docs, whose calls the build has checked
 * against the reference, and each engine's guide and API pages follow it.
 */
export function skillMarkdown(site: Site): string {
	return [
		'---',
		`name: ${skillName}`,
		// JSON's string is YAML's double-quoted one, and the summary holds a colon
		`description: ${JSON.stringify(skillDescription(site))}`,
		'---',
		'',
		'# TinyStore',
		'',
		site.summary,
		'',
		'Each block below is the first example of a page of the docs, in TypeScript, Python, Go and',
		"Rust one after another: read the program's language. The pages are linked as markdown.",
		'',
		'## Install',
		'',
		firstExamples(pageOf(site, installPage)),
		`The first program: ${site.origin}${pageOf(site, installPage).url}.md`,
		'',
		...apiEngines.flatMap(engine => engineSection(site, engine)),
		'## Read more',
		'',
		...llmsFiles(site),
		'',
	].join('\n')
}

/** /.well-known/agent-skills/index.json, which lists the skill with the SHA-256 of its bytes. */
export function skillIndex(site: Site): string {
	const digest = createHash('sha256').update(skillMarkdown(site)).digest('hex')
	const index = {
		$schema: 'https://schemas.agentskills.io/discovery/0.2.0/schema.json',
		skills: [
			{
				name: skillName,
				type: 'skill-md',
				description: skillDescription(site),
				url: `${site.origin}/.well-known/agent-skills/${skillName}/SKILL.md`,
				digest: `sha256:${digest}`,
			},
		],
	}
	return `${JSON.stringify(index, null, '\t')}\n`
}

function skillDescription(site: Site): string {
	return `${site.summary} Use it to write, read or debug a program that keeps its data in TinyStore, in TypeScript, Python, Go or Rust.`
}

function engineSection(site: Site, engine: ApiEngine): string[] {
	const guide = pageOf(site, `docs/${engine.guide}`)
	const api = apiLanguages.map(language => {
		const page = pageOf(site, apiFile(language, engine.slug))
		return `[${apiLanguageNames[language]}](${site.origin}${page.url}.md)`
	})
	return [
		`## ${engine.title}`,
		'',
		...(guide.lead === undefined ? [] : [guide.lead, '']),
		firstExamples(guide),
		`The guide: ${site.origin}${guide.url}.md. The API: ${api.join(', ')}.`,
		'',
	]
}

// the page's first blocks of code, one after another: a language each
function firstExamples(page: Page): string {
	const nodes = parse(page.markdown).children
	const first = nodes.findIndex(node => node.type === 'code')
	if (first === -1) {
		throw new Error(`${page.file} has no example for skill.md to show`)
	}
	const after = nodes.findIndex((node, at) => at > first && node.type !== 'code')
	return stringify(nodes.slice(first, after === -1 ? undefined : after))
}

function pageOf(site: Site, file: string): Page {
	const page = site.pages.find(candidate => candidate.file === file)
	if (page === undefined) {
		throw new Error(`skill.md shows ${file}, which the site does not build`)
	}
	return page
}
