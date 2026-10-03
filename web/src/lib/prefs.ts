// The reader's two choices, kept in this browser alone. app.html reads them
// back before the first paint; storage may be refused, and the page still works.

export type Theme = 'light' | 'dark'

const themeKey = 'docs.theme'
const languageKey = 'docs.language'

export function currentTheme(): Theme {
	const chosen = document.documentElement.dataset.theme
	if (chosen === 'light' || chosen === 'dark') {
		return chosen
	}
	return matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}

export function setTheme(theme: Theme): void {
	document.documentElement.dataset.theme = theme
	keep(themeKey, theme)
}

/** Every code block on every page shows this language from now on, where it has it. */
export function setLanguage(language: string): void {
	document.documentElement.dataset.lang = language
	keep(languageKey, language)
}

function keep(key: string, value: string): void {
	try {
		localStorage.setItem(key, value)
	} catch {
		// a private window or blocked storage: the choice lasts this page
	}
}
