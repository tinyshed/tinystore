import type { Element, ElementContent } from 'hast'

import { languageLabels, languages } from './code'

/** What separates a name's spellings in its inline code, in the switch's order. */
const separator = ' / '

/**
 * A name spelled in each language, written `defaultTtl / default_ttl /
 * DefaultTTL / default_ttl`: Bun's, Python's, Go's and Rust's, in the
 * switch's order. The page shows the spelling of the language the reader
 * picked, Bun's until one is, and the others in its title; GitHub and the
 * markdown copy show all four. Any other inline code is undefined here.
 */
export function languageNames(value: string): Element | undefined {
	const spellings = value.split(separator)
	if (spellings.length !== languages.length || spellings.some(spelling => spelling.trim() === '')) {
		return undefined
	}
	const spelled = languages.map((language, index) => ({
		language,
		spelling: spellings[index] ?? '',
	}))
	const children: ElementContent[] = spelled.flatMap(({ language, spelling }, index) => {
		const shown: Element = {
			type: 'element',
			tagName: 'span',
			properties: { dataLang: language },
			children: [{ type: 'text', value: spelling }],
		}
		// the separators stay in the text, so that a search finds every spelling apart
		const between: Element = {
			type: 'element',
			tagName: 'span',
			properties: { className: ['names-or'] },
			children: [{ type: 'text', value: separator }],
		}
		return index === 0 ? [shown] : [between, shown]
	})
	const title = spelled
		.map(({ language, spelling }) => `${languageLabels[language]}: ${spelling}`)
		.join(' · ')
	return { type: 'element', tagName: 'code', properties: { className: ['names'], title }, children }
}
