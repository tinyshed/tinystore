import { readFileSync } from 'node:fs'
import { extname, join } from 'node:path'
import { error } from '@sveltejs/kit'

import { checkout } from '$lib/content/repo'
import { loadSite } from '$lib/content/site'

import type { EntryGenerator, RequestHandler } from './$types'

export const prerender = true

const types: Record<string, string> = {
	'.svg': 'image/svg+xml',
	'.png': 'image/png',
	'.jpg': 'image/jpeg',
	'.jpeg': 'image/jpeg',
	'.gif': 'image/gif',
	'.webp': 'image/webp',
}

// the files a page shows, copied from the repository as they are
export const entries: EntryGenerator = async () => (await loadSite()).assets.map(path => ({ path }))

export const GET: RequestHandler = async ({ params }) => {
	if (!(await loadSite()).assets.includes(params.path)) {
		error(404, 'No such file')
	}
	return new Response(readFileSync(join(checkout(), params.path)), {
		headers: { 'content-type': types[extname(params.path)] ?? 'application/octet-stream' },
	})
}
