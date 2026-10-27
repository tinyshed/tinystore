import { markdownFile } from '$lib/content/markdown'
import { loadSite } from '$lib/content/site'

import type { RequestHandler } from './$types'

export const prerender = true

export const GET: RequestHandler = async () => {
	const site = await loadSite()
	const index = site.pages.find(page => page.slug === '')
	return new Response(markdownFile(index?.markdown ?? '', site.origin), {
		headers: { 'content-type': 'text/markdown; charset=utf-8' },
	})
}
