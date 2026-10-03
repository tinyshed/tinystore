import { siteOrigin } from '$lib/content/site'

import type { RequestHandler } from './$types'

export const prerender = true

// every crawler is welcome, the ones that feed an agent included
export const GET: RequestHandler = () =>
	new Response(`User-agent: *\nAllow: /\n\nSitemap: ${siteOrigin()}/sitemap.xml\n`, {
		headers: { 'content-type': 'text/plain; charset=utf-8' },
	})
