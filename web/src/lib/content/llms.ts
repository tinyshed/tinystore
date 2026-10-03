import type { Site } from './site'

/**
 * llms.txt, as llmstxt.org proposes it: the project in a line, then each
 * section of the index with its pages as markdown an agent can fetch.
 */
export function llmsIndex(site: Site): string {
	const lines = ['# TinyStore', '', `> ${site.summary}`, '']
	lines.push(
		'Every page below is also one file: llms-full.txt. Add .md to any page of the site',
		'for its markdown.',
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

/** Every page's markdown in the index's order, each after the address it is read at. */
export function llmsFull(site: Site): string {
	const parts = [`# TinyStore\n\n> ${site.summary}\n`]
	for (const page of site.pages.filter(page => page.hidden !== true)) {
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
