import { llmsNotFound } from '$lib/content/llms'
import { loadSite } from '$lib/content/site'

import type { RequestHandler } from './$types'

export const prerender = true

export const GET: RequestHandler = async () =>
	new Response(llmsNotFound(await loadSite()), {
		headers: { 'content-type': 'text/markdown; charset=utf-8' },
	})
