import { error } from '@sveltejs/kit'

import { loadSite } from '$lib/content/site'

import type { EntryGenerator, RequestHandler } from './$types'

export const prerender = true

export const entries: EntryGenerator = async () =>
	(await loadSite()).pages
		.filter(page => page.slug !== '')
		.map(page => ({ file: `${page.slug}.md` }))

export const GET: RequestHandler = async ({ params }) => {
	const slug = params.file.replace(/\.md$/, '')
	const page = (await loadSite()).pages.find(candidate => candidate.slug === slug)
	if (page === undefined) {
		error(404, 'No such page')
	}
	return new Response(page.markdown, {
		headers: { 'content-type': 'text/markdown; charset=utf-8' },
	})
}
