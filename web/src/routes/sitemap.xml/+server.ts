import { sitemap } from '$lib/content/llms'
import { loadSite } from '$lib/content/site'

import type { RequestHandler } from './$types'

export const prerender = true

export const GET: RequestHandler = async () =>
	new Response(sitemap(await loadSite()), {
		headers: { 'content-type': 'application/xml; charset=utf-8' },
	})
