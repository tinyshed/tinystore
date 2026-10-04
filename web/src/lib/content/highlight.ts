import githubDark from '@shikijs/themes/github-dark-default'
import githubLight from '@shikijs/themes/github-light-default'
import type { Element } from 'hast'
import { createHighlighter, type LanguageRegistration, type ThemeRegistration } from 'shiki'

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
	log: 'tinystore-log',
	dockerfile: 'dockerfile',
	html: 'html',
	css: 'css',
	svelte: 'svelte',
}

/**
 * The lines a logger of the store prints on a terminal, coloured as
 * records/console.go colours them, so that a fence of them reads as it would:
 *
 *     11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200
 *     dim          yellow cyan              dim=           dim=
 *
 * The time may be in full or left out, and so may the stream, the one word
 * that two spaces follow. A line indented by four spaces, a stack, is dim.
 */
const consoleLog: LanguageRegistration = {
	name: 'tinystore-log',
	scopeName: 'source.tinystore-log',
	repository: {},
	patterns: [
		{
			match:
				'^(?:(\\d{4}-\\d\\d-\\d\\d \\d\\d:\\d\\d:\\d\\d\\.\\d{3} [+-]\\d\\d:\\d\\d|\\d\\d:\\d\\d:\\d\\d\\.\\d{3}) +)?' +
				'(?:(DEBUG\\S*)|(INFO\\S*)|(WARN\\S*)|(ERROR\\S*)|(EVENT))(?: +([^\\s=]+)(?=  |$))?',
			captures: {
				1: { name: 'log.dim' },
				2: { name: 'log.blue' },
				3: { name: 'log.green' },
				4: { name: 'log.yellow' },
				5: { name: 'log.red' },
				6: { name: 'log.magenta' },
				7: { name: 'log.cyan' },
			},
		},
		{ match: '(?<= )(?:"(?:[^"\\\\]|\\\\.)*"|[^\\s="]+)=', name: 'log.dim' },
		{ match: '^    .*$', name: 'log.dim' },
	],
}

// the colours console.go writes, as the theme's terminal shows them
const terminalColour = {
	blue: 'terminal.ansiBlue',
	green: 'terminal.ansiGreen',
	yellow: 'terminal.ansiYellow',
	red: 'terminal.ansiRed',
	magenta: 'terminal.ansiMagenta',
	cyan: 'terminal.ansiCyan',
} as const
const consoleColours = Object.keys(terminalColour) as (keyof typeof terminalColour)[]

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
		langs: [
			...new Set(Object.values(grammars).filter(name => name !== consoleLog.name)),
			consoleLog,
		],
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
		tokenColors: [
			...(theme.tokenColors ?? []).map(rule => {
				if ([rule.scope].flat().includes('comment')) {
					return { ...rule, settings: { ...rule.settings, foreground: colors.comment } }
				}
				// a rule that repeats the theme's text colour follows the design's text
				if (rule.settings.foreground?.toLowerCase() === text) {
					return { ...rule, settings: { ...rule.settings, foreground: colors.text } }
				}
				return rule
			}),
			{ scope: 'log.dim', settings: { foreground: colors.comment } },
			...consoleColours.map(colour => ({
				scope: `log.${colour}`,
				settings: { foreground: theme.colors?.[terminalColour[colour]] },
			})),
		],
	}
}
