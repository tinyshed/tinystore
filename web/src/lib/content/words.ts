/**
 * A command line's words as a POSIX shell reads them: blanks between words,
 * `'…'` taken as it is, `"…"` with a backslash before `"`, `\` or `$`, and a
 * backslash outside quotes keeping the next character. What the site's
 * explain reads a typed command with, and what `tools/cli-json.ts` runs an
 * example with.
 *
 *     tinystore jobs ./data add emails '{"to": "ada"}'   tinystore · jobs · ./data · add · emails · {"to": "ada"}
 */
export function words(line: string): string[] {
	const found: string[] = []
	let word: string | undefined
	for (let at = 0; at < line.length; at++) {
		const character = line[at] ?? ''
		if (/\s/.test(character)) {
			if (word !== undefined) found.push(word)
			word = undefined
			continue
		}
		word ??= ''
		if (character === "'") {
			const end = line.indexOf("'", at + 1)
			word += line.slice(at + 1, end < 0 ? line.length : end)
			at = end < 0 ? line.length : end
		} else if (character === '"') {
			at++
			while (at < line.length && line[at] !== '"') {
				if (line[at] === '\\' && /["\\$`]/.test(line[at + 1] ?? '')) at++
				word += line[at] ?? ''
				at++
			}
		} else if (character === '\\') {
			at++
			word += line[at] ?? ''
		} else {
			word += character
		}
	}
	if (word !== undefined) found.push(word)
	return found
}
