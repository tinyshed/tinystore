import { error } from '@sveltejs/kit'

import { loadSite } from '$lib/content/site'
import { skillMarkdown, skillName } from '$lib/content/skill'

import type { EntryGenerator, RequestHandler } from './$types'

export const prerender = true

// the same bytes as /skill.md, where the discovery index says skills are
export const entries: EntryGenerator = () => [{ skill: skillName }]

export const GET: RequestHandler = async ({ params }) => {
	if (params.skill !== skillName) {
		error(404, 'No such skill')
	}
	return new Response(skillMarkdown(await loadSite()), {
		headers: { 'content-type': 'text/markdown; charset=utf-8' },
	})
}
