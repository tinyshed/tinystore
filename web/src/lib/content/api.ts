/**
 * The API pages' names and places, shared by their generator (reference.ts)
 * and the site, which lists them under one sidebar entry: the index at
 * docs/reference/api.md, and docs/reference/<language>/<engine>.md beside it.
 */

export const apiLanguages = ['bun', 'python', 'go', 'rust'] as const

export type ApiLanguage = (typeof apiLanguages)[number]

export const apiLanguageNames: Record<ApiLanguage, string> = {
	bun: 'Bun and Node',
	python: 'Python',
	go: 'Go',
	rust: 'Rust',
}

export interface ApiEngine {
	slug: string
	title: string
	/** the guide a page sends its reader to, as a path of docs/ */
	guide: string
}

export const apiEngines: ApiEngine[] = [
	{ slug: 'store', title: 'Store', guide: 'start/directory.md' },
	{ slug: 'sql', title: 'SQL', guide: 'chat/messages.md' },
	{ slug: 'kv', title: 'KV', guide: 'chat/sessions-and-limits.md' },
	{ slug: 'jobs', title: 'Jobs', guide: 'chat/replies.md' },
	{ slug: 'blobs', title: 'Blobs', guide: 'chat/attachments.md' },
	{ slug: 'search', title: 'Search', guide: 'chat/finding-old-messages.md' },
	{ slug: 'channels', title: 'Channels', guide: 'chat/live-updates.md' },
	{ slug: 'locks', title: 'Locks', guide: 'chat/live-updates.md#one-summary-per-chat-at-a-time' },
	{ slug: 'config', title: 'Config', guide: 'chat/settings.md' },
	{ slug: 'records', title: 'Records', guide: 'chat/what-it-is-doing.md' },
	{ slug: 'metrics', title: 'Metrics', guide: 'start/why.md#the-engines' },
	{ slug: 'backup', title: 'Backup', guide: 'start/why.md#the-engines' },
]

export const apiIndexFile = 'docs/reference/api.md'

export function apiFile(language: ApiLanguage, engine: string): string {
	return `docs/reference/${language}/${engine}.md`
}

/** A generated API page's language and engine, or undefined for any other file. */
export function apiPageOf(file: string): { language: ApiLanguage; engine: string } | undefined {
	const match = /^docs\/reference\/(go|bun|python|rust)\/([^/]+)\.md$/.exec(file)
	if (match === null) {
		return undefined
	}
	return { language: match[1] as ApiLanguage, engine: match[2] as string }
}
