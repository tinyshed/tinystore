/**
 * The API pages' names and places, shared by their generator (reference.ts)
 * and the site, which lists them under one sidebar entry: the index at
 * docs/reference/api.md, and docs/reference/<language>/<engine>.md beside it.
 */

export const apiLanguages = ['bun', 'python', 'go'] as const

export type ApiLanguage = (typeof apiLanguages)[number]

export const apiLanguageNames: Record<ApiLanguage, string> = {
	bun: 'Bun and Node',
	python: 'Python',
	go: 'Go',
}

export interface ApiEngine {
	slug: string
	title: string
	/** the guide a page sends its reader to, as a path of docs/ */
	guide: string
}

export const apiEngines: ApiEngine[] = [
	{ slug: 'store', title: 'Store', guide: 'languages.md' },
	{ slug: 'sql', title: 'SQL', guide: 'sql/README.md' },
	{ slug: 'kv', title: 'KV', guide: 'kv/README.md' },
	{ slug: 'jobs', title: 'Jobs', guide: 'jobs/README.md' },
	{ slug: 'blobs', title: 'Blobs', guide: 'blobs/README.md' },
	{ slug: 'records', title: 'Records', guide: 'records/README.md' },
	{ slug: 'metrics', title: 'Metrics', guide: 'metrics/README.md' },
]

export const apiIndexFile = 'docs/reference/api.md'

export function apiFile(language: ApiLanguage, engine: string): string {
	return `docs/reference/${language}/${engine}.md`
}

/** A generated API page's language and engine, or undefined for any other file. */
export function apiPageOf(file: string): { language: ApiLanguage; engine: string } | undefined {
	const match = /^docs\/reference\/(go|bun|python)\/([^/]+)\.md$/.exec(file)
	if (match === null) {
		return undefined
	}
	return { language: match[1] as ApiLanguage, engine: match[2] as string }
}
