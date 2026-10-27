import { loadSite } from '$lib/content/site'
import { skillIndex } from '$lib/content/skill'

import type { RequestHandler } from './$types'

export const prerender = true

export const GET: RequestHandler = async () =>
	new Response(skillIndex(await loadSite()), {
		headers: { 'content-type': 'application/json; charset=utf-8' },
	})
