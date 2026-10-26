// The reader's two choices, kept in this browser alone. app.html reads them
// back before the first paint; storage may be refused, and the page still works.

export type Theme = 'light' | 'dark'

/** A theme and its tone: warm paper under the light one, true black under the dark one. */
export type Look = Theme | 'paper' | 'black'

const themeKey = 'docs.theme'
const toneKey = 'docs.tone'
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

export function currentLook(): Look {
	const theme = currentTheme()
	const tone = document.documentElement.dataset.tone
	if (theme === 'light' && tone === 'paper') {
		return 'paper'
	}
	return theme === 'dark' && tone === 'black' ? 'black' : theme
}

/** Sets the theme, and the tone over it, or none. */
export function setLook(look: Look): void {
	setTheme(look === 'paper' ? 'light' : look === 'black' ? 'dark' : look)
	const root = document.documentElement
	if (look === 'paper' || look === 'black') {
		root.dataset.tone = look
		keep(toneKey, look)
	} else {
		delete root.dataset.tone
		forget(toneKey)
	}
}

/** Every code block on every page shows this language from now on, where it has it. */
export function setLanguage(language: string): void {
	document.documentElement.dataset.lang = language
	keep(languageKey, language)
}

function forget(key: string): void {
	try {
		localStorage.removeItem(key)
	} catch {
		// nothing was kept where storage is refused
	}
}

function keep(key: string, value: string): void {
	try {
		localStorage.setItem(key, value)
	} catch {
		// a private window or blocked storage: the choice lasts this page
	}
}
