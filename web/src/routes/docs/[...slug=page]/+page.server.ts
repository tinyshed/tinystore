import { error } from '@sveltejs/kit'

import { loadSite } from '$lib/content/site'

import type { EntryGenerator, PageServerLoad } from './$types'

export const entries: EntryGenerator = async () =>
	(await loadSite()).pages.map(page => ({ slug: page.slug }))

export const load: PageServerLoad = async ({ params }) => {
	const page = (await loadSite()).pages.find(candidate => candidate.slug === params.slug)
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
		},
	}
}
