import { loadSite } from '$lib/content/site'
import type { Entry } from '$lib/search'

import type { RequestHandler } from './$types'

export const prerender = true

export const GET: RequestHandler = async () => {
	const site = await loadSite()
	const entries: Entry[] = site.pages
		.filter(page => page.hidden !== true)
		.flatMap(page =>
			page.sections.map(section => ({
				page: page.title,
				heading: section.heading,
				url: section.id === undefined ? page.url : `${page.url}#${section.id}`,
				kind: page.kind,
				text: section.text,
				code: section.code,
			})),
		)
	return Response.json(entries)
}
