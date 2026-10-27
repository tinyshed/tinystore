import { siteOrigin } from '$lib/content/site'

import type { RequestHandler } from './$types'

export const prerender = true

// every crawler is welcome, the ones that feed an agent included; Content-Signal
// says, as Cloudflare's content signals do, that the docs may train a model,
// be found by a search and be read to answer a question
export const GET: RequestHandler = () =>
	new Response(
		[
			'User-agent: *',
			'Content-Signal: ai-train=yes, search=yes, ai-input=yes',
			'Allow: /',
			'',
			`Sitemap: ${siteOrigin()}/sitemap.xml`,
			'',
		].join('\n'),
		{ headers: { 'content-type': 'text/plain; charset=utf-8' } },
	)
