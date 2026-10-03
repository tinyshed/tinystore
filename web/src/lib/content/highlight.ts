import githubDark from '@shikijs/themes/github-dark-default'
import githubLight from '@shikijs/themes/github-light-default'
import type { Element } from 'hast'
import { createHighlighter, type ThemeRegistration } from 'shiki'

// what a fence may say, and the grammar it means; anything else is shown as plain text
const grammars: Record<string, string> = {
	go: 'go',
	ts: 'typescript',
	typescript: 'typescript',
	js: 'javascript',
	javascript: 'javascript',
	python: 'python',
	py: 'python',
	sh: 'shellscript',
	bash: 'shellscript',
	shell: 'shellscript',
	console: 'shellscript',
	sql: 'sql',
	json: 'json',
	yaml: 'yaml',
	yml: 'yaml',
	toml: 'toml',
	diff: 'diff',
	dockerfile: 'dockerfile',
	html: 'html',
	css: 'css',
	svelte: 'svelte',
}

/**
 * GitHub's palettes, which the design takes its code colours from, with its
 * own text and comment colours over them.
 */
const themes = {
	dark: retint(githubDark, 'tinystore-dark', { text: '#eceaf3', comment: '#7f7c8f' }),
	light: retint(githubLight, 'tinystore-light', { text: '#1f2328', comment: '#6e7781' }),
}

export type Highlight = (code: string, lang: string) => Element

/**
 * A highlighter of the fences the docs use. Each token carries both themes'
 * colours as --shiki-light and --shiki-dark, and the stylesheet picks one, so
 * the page needs no script to highlight and none to change theme.
 */
export async function createHighlight(): Promise<Highlight> {
	const highlighter = await createHighlighter({
		themes: [themes.dark, themes.light],
		// Oniguruma, the grammars' own engine: the JavaScript one tokenized a first fence wrongly
		langs: [...new Set(Object.values(grammars))],
	})

	return (code, lang) => {
		const root = highlighter.codeToHast(code, {
			lang: grammars[lang] ?? 'text',
			themes: { light: 'tinystore-light', dark: 'tinystore-dark' },
			defaultColor: false,
		})
		const pre = root.children[0]
		if (pre?.type !== 'element') {
			throw new Error(`shiki gave no <pre> for a ${lang} fence`)
		}
		// the stylesheet owns the block's background and focus, not the theme
		pre.properties = { className: ['shiki'] }
		return pre
	}
}

function retint(
	theme: ThemeRegistration,
	name: string,
	colors: { text: string; comment: string },
): ThemeRegistration {
	const text = theme.colors?.['editor.foreground']?.toLowerCase()

	return {
		...theme,
		name,
		colors: { ...theme.colors, 'editor.foreground': colors.text },
		tokenColors: (theme.tokenColors ?? []).map(rule => {
			if ([rule.scope].flat().includes('comment')) {
				return { ...rule, settings: { ...rule.settings, foreground: colors.comment } }
			}
			// a rule that repeats the theme's text colour follows the design's text
			if (rule.settings.foreground?.toLowerCase() === text) {
				return { ...rule, settings: { ...rule.settings, foreground: colors.text } }
			}
			return rule
		}),
	}
}
