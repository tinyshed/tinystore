// The reader's two choices, kept in this browser alone. app.html reads them
// back before the first paint; storage may be refused, and the page still works.

export type Theme = 'light' | 'dark'

/** The looks a reader picks from the theme's menu, each on a base theme. */
export const looks = {
	light: { label: 'TinyStore', theme: 'light' },
	paper: { label: 'Paper', theme: 'light' },
	'github-light': { label: 'GitHub Light', theme: 'light' },
	'solarized-light': { label: 'Solarized Light', theme: 'light' },
	'rose-pine-dawn': { label: 'Rosé Pine Dawn', theme: 'light' },
	dark: { label: 'TinyStore', theme: 'dark' },
	black: { label: 'Black', theme: 'dark' },
	'github-dark': { label: 'GitHub Dark', theme: 'dark' },
	dracula: { label: 'Dracula', theme: 'dark' },
	nord: { label: 'Nord', theme: 'dark' },
	'tokyo-night': { label: 'Tokyo Night', theme: 'dark' },
	'catppuccin-mocha': { label: 'Catppuccin Mocha', theme: 'dark' },
} as const satisfies Record<string, { label: string; theme: Theme }>

export type Look = keyof typeof looks

const themeKey = 'docs.theme'
const lookKey = 'docs.look'
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
	const look = document.documentElement.dataset.look
	return look !== undefined && look in looks ? (look as Look) : currentTheme()
}

/** Sets the look and the theme it is drawn on; the design's own two need no look. */
export function setLook(look: Look): void {
	setTheme(looks[look].theme)
	const root = document.documentElement
	if (look === 'light' || look === 'dark') {
		delete root.dataset.look
		forget(lookKey)
	} else {
		root.dataset.look = look
		keep(lookKey, look)
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
