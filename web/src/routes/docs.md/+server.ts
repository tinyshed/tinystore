import { loadSite } from '$lib/content/site'

import type { RequestHandler } from './$types'

export const prerender = true

export const GET: RequestHandler = async () => {
	const index = (await loadSite()).pages.find(page => page.slug === '')
	return new Response(index?.markdown ?? '', {
		headers: { 'content-type': 'text/markdown; charset=utf-8' },
	})
}
