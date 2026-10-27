import { type ApiLanguage, apiLanguageNames, apiLanguages, apiPageOf } from './api'
import type { Site } from './site'

/**
 * llms.txt, as llmstxt.org proposes it: the project in a line, then each
 * section of the index with its pages as markdown an agent can fetch.
 */
export function llmsIndex(site: Site): string {
	const lines = ['# TinyStore', '', `> ${site.summary}`, '']
	lines.push(
		'Every guide below is also one file: llms-full.txt. The API pages, every public name',
		'with its signature, are one file per language instead: ' +
			apiLanguages.map(language => `${site.origin}/llms-${language}.txt`).join(', ') +
			'. Add .md to any page of the site for its markdown, or ask for the page with',
		'Accept: text/markdown.',
		'',
	)

	for (const section of site.nav) {
		const pages = section.items.flatMap(item =>
			item.kind === 'page' ? [site.pages.find(page => page.file === item.file)] : [],
		)
		const listed = pages.filter(page => page !== undefined)
		if (listed.length === 0) {
			continue
		}
		lines.push(`## ${section.title}`, '')
		for (const page of listed) {
			lines.push(
				`- [${page.title}](${site.origin}${page.url}.md)${page.lead ? `: ${page.lead}` : ''}`,
			)
		}
		lines.push('')
	}

	return `${lines.join('\n').trimEnd()}\n`
}

/**
 * Every guide's markdown in the index's order, each after the address it is
 * read at. The API pages are left to llmsApi: three languages' signatures
 * would double the file, and an agent needs one of them.
 */
export function llmsFull(site: Site): string {
	return joined(
		site,
		site.pages.filter(page => page.hidden !== true && apiPageOf(page.file) === undefined),
	)
}

/** One language's API pages as one file, llms-<language>.txt. */
export function llmsApi(site: Site, language: ApiLanguage): string {
	return joined(
		site,
		site.pages.filter(page => apiPageOf(page.file)?.language === language),
	)
}

/**
 * 404.md, what the server answers a request for markdown that finds nothing:
 * where every page is listed, so that an agent's wrong guess is not a dead end.
 */
export function llmsNotFound(site: Site): string {
	return [
		'# Not found',
		'',
		'No page of the TinyStore docs is at this address. It may have moved when the docs were rewritten.',
		'',
		...llmsFiles(site),
		'',
	].join('\n')
}

/** The files an agent reads the docs through, a markdown list item each. */
export function llmsFiles(site: Site): string[] {
	const api = apiLanguages.map(
		language => `[${apiLanguageNames[language]}](${site.origin}/llms-${language}.txt)`,
	)
	return [
		`- [llms.txt](${site.origin}/llms.txt): every page with a line on it, each as markdown`,
		`- [llms-full.txt](${site.origin}/llms-full.txt): every guide in one file`,
		`- [The docs index](${site.origin}/docs.md): the pages as the site's sidebar lists them`,
		`- The API, one file per language: ${api.join(', ')}`,
	]
}

function joined(site: Site, pages: Site['pages']): string {
	const parts = [`# TinyStore\n\n> ${site.summary}\n`]
	for (const page of pages) {
		parts.push(`---\n\nSource: ${site.origin}${page.url}\n\n${page.markdown.trim()}\n`)
	}
	return parts.join('\n')
}

export function sitemap(site: Site): string {
	const urls = [{ url: '/' }, ...site.pages.filter(page => page.hidden !== true)].map(page => {
		const updated =
			'updated' in page && page.updated !== undefined ? `<lastmod>${page.updated}</lastmod>` : ''
		return `  <url><loc>${site.origin}${page.url}</loc>${updated}</url>`
	})
	return [
		'<?xml version="1.0" encoding="UTF-8"?>',
		'<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">',
		...urls,
		'</urlset>',
		'',
	].join('\n')
}
