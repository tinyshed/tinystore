import { type ApiLanguage, apiLanguages } from '$lib/content/api'
import { llmsApi } from '$lib/content/llms'
import { loadSite } from '$lib/content/site'

import type { EntryGenerator, RequestHandler } from './$types'

export const prerender = true

export const entries: EntryGenerator = () => apiLanguages.map(language => ({ language }))

export const GET: RequestHandler = async ({ params }) =>
	new Response(llmsApi(await loadSite(), params.language as ApiLanguage), {
		headers: { 'content-type': 'text/plain; charset=utf-8' },
	})
