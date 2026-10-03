import { llmsIndex } from '$lib/content/llms'
import { loadSite } from '$lib/content/site'

import type { RequestHandler } from './$types'

export const prerender = true

export const GET: RequestHandler = async () =>
	new Response(llmsIndex(await loadSite()), {
		headers: { 'content-type': 'text/plain; charset=utf-8' },
	})
