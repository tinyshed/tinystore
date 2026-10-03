/** A part of a page as the search finds it: /search.json holds one per heading. */
export interface Entry {
	page: string
	heading: string
	url: string
	kind: 'guide' | 'reference' | 'design'
	text: string
	code: string
}

export type Filter = 'all' | Entry['kind'] | 'code'

export interface Hit {
	entry: Entry
	score: number
	/** HTML: the text around the first match, every match in <mark> */
	snippet: string
}

/**
 * The entries holding every word of the query, best first. A word in a heading
 * counts most, then in the page's title, then in the text; the Code filter
 * looks in code alone. Small enough to need no index: the docs are a few
 * thousand sections at most, scanned on each keystroke.
 */
export function find(entries: Entry[], query: string, filter: Filter, limit = 30): Hit[] {
	const words = [
		...new Set(
			query
				.toLowerCase()
				.split(/\s+/)
				.filter(word => word !== ''),
		),
	]
	if (words.length === 0) {
		return []
	}

	const hits: Hit[] = []
	for (const entry of entries) {
		if (filter !== 'all' && filter !== 'code' && entry.kind !== filter) {
			continue
		}
		const score = scoreOf(entry, words, filter === 'code')
		if (score > 0) {
			const inText =
				filter !== 'code' && words.some(word => entry.text.toLowerCase().includes(word))
			hits.push({ entry, score, snippet: snippet(inText ? entry.text : entry.code, words) })
		}
	}

	return hits.sort((a, b) => b.score - a.score).slice(0, limit)
}

function scoreOf(entry: Entry, words: string[], codeOnly: boolean): number {
	const heading = entry.heading.toLowerCase()
	const page = entry.page.toLowerCase()
	const text = entry.text.toLowerCase()
	const code = entry.code.toLowerCase()

	let score = 0
	for (const word of words) {
		if (codeOnly) {
			if (!code.includes(word)) {
				return 0
			}
			score += 2
			continue
		}
		const inHeading = heading.includes(word)
		const inPage = page.includes(word)
		const inText = text.includes(word)
		const inCode = code.includes(word)
		if (!inHeading && !inPage && !inText && !inCode) {
			return 0
		}
		score += (inHeading ? 8 : 0) + (startsWord(heading, word) ? 4 : 0) + (inPage ? 3 : 0)
		score += (inText ? 1 : 0) + (inCode ? 0.5 : 0)
	}
	// a guide answers before the design behind it
	return score + (entry.kind === 'guide' ? 0.5 : 0)
}

function startsWord(text: string, word: string): boolean {
	return new RegExp(`(^|[^a-z0-9])${escapeRegExp(word)}`).test(text)
}

/** The text around the first match, about 160 characters, in HTML with every match marked. */
export function snippet(text: string, words: string[]): string {
	const lower = text.toLowerCase()
	const first = Math.min(...words.map(word => lower.indexOf(word)).filter(at => at >= 0))
	if (!Number.isFinite(first)) {
		return escapeHtml(text.slice(0, 160))
	}

	let start = Math.max(0, first - 50)
	let end = Math.min(text.length, first + 110)
	start = start === 0 ? 0 : text.indexOf(' ', start) + 1 || start
	end = end === text.length ? end : text.lastIndexOf(' ', end) || end
	const window = text.slice(start, end)

	const pattern = new RegExp(words.map(escapeRegExp).join('|'), 'gi')
	let html = ''
	let last = 0
	for (const match of window.matchAll(pattern)) {
		html += `${escapeHtml(window.slice(last, match.index))}<mark>${escapeHtml(match[0])}</mark>`
		last = match.index + match[0].length
	}
	html += escapeHtml(window.slice(last))

	return `${start > 0 ? '…' : ''}${html}${end < text.length ? '…' : ''}`
}

function escapeHtml(text: string): string {
	return text.replace(/[&<>"']/g, char => `&#${char.charCodeAt(0)};`)
}

function escapeRegExp(text: string): string {
	return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
