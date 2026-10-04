import { error } from '@sveltejs/kit'

import { apiEngines, apiFile, apiLanguageNames, apiLanguages, apiPageOf } from '$lib/content/api'
import { loadSite, type Site } from '$lib/content/site'

import type { EntryGenerator, PageServerLoad } from './$types'

export const entries: EntryGenerator = async () =>
	(await loadSite()).pages.map(page => ({ slug: page.slug }))

export const load: PageServerLoad = async ({ params }) => {
	const site = await loadSite()
	const page = site.pages.find(candidate => candidate.slug === params.slug)
	if (page === undefined) {
		error(404, 'No such page')
	}

	// what the page draws, and not its markdown or search text, which have files of their own
	return {
		page: {
			url: page.url,
			title: page.title,
			lead: page.lead,
			html: page.html,
			headings: page.headings,
			section: page.section,
			updated: page.updated,
			editUrl: page.editUrl,
			markdown: `${page.url}.md`,
			previous: page.previous,
			next: page.next,
			hidden: page.hidden === true,
			api: apiSwitch(site, page.file),
		},
	}
}

/**
 * An API page's two switches: the same engine in the other languages, and
 * the other engines in the same language.
 */
function apiSwitch(site: Site, file: string) {
	const here = apiPageOf(file)
	if (here === undefined) {
		return undefined
	}
	const urlOf = (wanted: string) => site.pages.find(page => page.file === wanted)?.url
	const languages = apiLanguages.flatMap(language => {
		const url = urlOf(apiFile(language, here.engine))
		return url === undefined ? [] : [{ key: language, label: apiLanguageNames[language], url }]
	})
	const engines = apiEngines.flatMap(engine => {
		const url = urlOf(apiFile(here.language, engine.slug))
		return url === undefined ? [] : [{ key: engine.slug, label: engine.title, url }]
	})
	return { language: here.language, engine: here.engine, languages, engines }
}
