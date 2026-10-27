import { loadSite } from '$lib/content/site'
import { skillMarkdown } from '$lib/content/skill'

import type { RequestHandler } from './$types'

export const prerender = true

export const GET: RequestHandler = async () =>
	new Response(skillMarkdown(await loadSite()), {
		headers: { 'content-type': 'text/markdown; charset=utf-8' },
	})
